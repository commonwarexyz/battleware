import React from 'react';

const RetroText = ({ children, className = "" }) => (
  <div className={`font-retro uppercase ${className}`}>
    {children}
  </div>
);

export default RetroText;