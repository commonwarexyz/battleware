import React, { useState, useEffect, useRef } from 'react';
import './MaintenancePage.css';

const MaintenancePage = () => {
  const containerRef = useRef(null);
  const logoRef = useRef(null);
  const positionRef = useRef({ x: 50, y: 50 });
  const directionRef = useRef({ x: 1, y: 1 });
  const [color, setColor] = useState('#0000ee');
  const currentColorRef = useRef('#0000ee');
  const animationFrameRef = useRef(null);
  const initializedRef = useRef(false);
  const speed = 0.5; // pixels per frame
  const logoDimensionsRef = useRef({ width: 0, height: 0 });

  const handleLogoClick = () => {
    window.open('https://x.com/commonwarexyz', '_blank', 'noopener,noreferrer');
  };

  useEffect(() => {
    const measureLogo = () => {
      if (logoRef.current && containerRef.current) {
        const rect = logoRef.current.getBoundingClientRect();
        logoDimensionsRef.current = {
          width: rect.width,
          height: rect.height,
        };
      }
    };

    measureLogo();
    const timer = setTimeout(measureLogo, 200);

    return () => clearTimeout(timer);
  }, []);

  useEffect(() => {
    const initTimeout = setTimeout(() => {
      if (!initializedRef.current && containerRef.current && logoRef.current) {
        const containerWidth = containerRef.current.clientWidth;
        const containerHeight = containerRef.current.clientHeight;

        if (logoDimensionsRef.current.width === 0) {
          const rect = logoRef.current.getBoundingClientRect();
          logoDimensionsRef.current = {
            width: rect.width,
            height: rect.height,
          };
        }

        const logoWidth = logoDimensionsRef.current.width;
        const logoHeight = logoDimensionsRef.current.height;

        positionRef.current = {
          x: Math.random() * (containerWidth - logoWidth),
          y: Math.random() * (containerHeight - logoHeight),
        };

        initializedRef.current = true;
        logoRef.current.style.left = `${positionRef.current.x}px`;
        logoRef.current.style.top = `${positionRef.current.y}px`;
      }
    }, 100);

    return () => clearTimeout(initTimeout);
  }, []);

  useEffect(() => {
    const colors = [
      '#0000ee',
      '#ee0000',
      '#00ee00',
      '#ee00ee',
      '#eeee00',
      '#00eeee',
      '#ff7700',
      '#7700ff',
    ];

    const getRandomColor = () => {
      const filteredColors = colors.filter((c) => c !== currentColorRef.current);
      return filteredColors[Math.floor(Math.random() * filteredColors.length)];
    };

    const updateColor = () => {
      const newColor = getRandomColor();
      currentColorRef.current = newColor;
      setColor(newColor);
    };

    const animate = () => {
      if (!containerRef.current || !logoRef.current) {
        animationFrameRef.current = requestAnimationFrame(animate);
        return;
      }

      const containerWidth = containerRef.current.clientWidth;
      const containerHeight = containerRef.current.clientHeight;
      const logoWidth = logoDimensionsRef.current.width;
      const logoHeight = logoDimensionsRef.current.height;

      let newX = positionRef.current.x + speed * directionRef.current.x;
      let newY = positionRef.current.y + speed * directionRef.current.y;
      let colorChanged = false;
      const rightEdgeThreshold = containerWidth - logoWidth;

      if (newX <= 0) {
        directionRef.current.x = Math.abs(directionRef.current.x);
        newX = 0;
        if (!colorChanged) {
          updateColor();
          colorChanged = true;
        }
      } else if (newX >= rightEdgeThreshold) {
        directionRef.current.x = -Math.abs(directionRef.current.x);
        newX = rightEdgeThreshold;
        if (!colorChanged) {
          updateColor();
          colorChanged = true;
        }
      }

      const bottomEdgeThreshold = containerHeight - logoHeight;
      if (newY <= 0) {
        directionRef.current.y = Math.abs(directionRef.current.y);
        newY = 0;
        if (!colorChanged) {
          updateColor();
          colorChanged = true;
        }
      } else if (newY >= bottomEdgeThreshold) {
        directionRef.current.y = -Math.abs(directionRef.current.y);
        newY = bottomEdgeThreshold;
        if (!colorChanged) {
          updateColor();
          colorChanged = true;
        }
      }

      positionRef.current = { x: newX, y: newY };

      logoRef.current.style.left = `${newX}px`;
      logoRef.current.style.top = `${newY}px`;

      animationFrameRef.current = requestAnimationFrame(animate);
    };

    animationFrameRef.current = requestAnimationFrame(animate);

    return () => {
      if (animationFrameRef.current !== null) {
        cancelAnimationFrame(animationFrameRef.current);
      }
    };
  }, []);

  useEffect(() => {
    const handleResize = () => {
      if (containerRef.current && logoRef.current) {
        const containerWidth = containerRef.current.clientWidth;
        const containerHeight = containerRef.current.clientHeight;
        const logoWidth = logoRef.current.clientWidth;
        const logoHeight = logoRef.current.clientHeight;

        let newX = positionRef.current.x;
        let newY = positionRef.current.y;

        if (newX + logoWidth > containerWidth) {
          newX = containerWidth - logoWidth;
        }

        if (newY + logoHeight > containerHeight) {
          newY = containerHeight - logoHeight;
        }

        positionRef.current = { x: newX, y: newY };
        logoRef.current.style.left = `${newX}px`;
        logoRef.current.style.top = `${newY}px`;
      }
    };

    window.addEventListener('resize', handleResize);
    return () => {
      window.removeEventListener('resize', handleResize);
    };
  }, []);

  return (
    <div className="dvd-container" ref={containerRef}>
      <div
        className="dvd-logo"
        ref={logoRef}
        style={{
          color: color,
          borderColor: color,
        }}
        onClick={handleLogoClick}
      >
        <div className="logo-content">
          <div className="maintenance-text">
            <p>SYSTEM MAINTENANCE</p>
            <p className="small-text">
              Follow <span className="link-text">@commonwarexyz</span> for updates and new releases.
            </p>
          </div>
        </div>
      </div>
    </div>
  );
};

export default MaintenancePage;
