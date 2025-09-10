use crate::{Client, Error, Result};
use battleware_types::{api::Query, Seed, NAMESPACE};
use commonware_codec::{DecodeExt, Encode};
use commonware_consensus::Viewable;
use url::Url;

fn query_seed_path(base: &Url, query: &Query) -> String {
    let query = query.encode();
    base.join(&format!("seed/{}", commonware_utils::hex(&query)))
        .unwrap()
        .to_string()
}

impl Client {
    pub async fn query_seed(&self, query: Query) -> Result<Option<Seed>> {
        // Make request
        let result = self
            .http_client
            .get(query_seed_path(&self.base_url, &query))
            .send()
            .await?;

        // Parse response
        match result.status() {
            reqwest::StatusCode::NOT_FOUND => Ok(None),
            reqwest::StatusCode::OK => {
                let bytes = result.bytes().await.map_err(Error::Reqwest)?;
                let seed = Seed::decode(bytes.as_ref()).map_err(Error::InvalidData)?;
                if !seed.verify(NAMESPACE, &self.identity) {
                    return Err(Error::InvalidSignature);
                }

                // Verify the seed matches the query
                match query {
                    Query::Latest => {}
                    Query::Index(index) => {
                        if seed.view() != index {
                            return Err(Error::UnexpectedResponse);
                        }
                    }
                }
                Ok(Some(seed))
            }
            _ => Err(Error::Failed(result.status())),
        }
    }
}
