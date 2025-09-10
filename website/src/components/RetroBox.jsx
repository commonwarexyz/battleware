import React from 'react';

const RetroBox = ({ children, className = "", dark = false }) => (
  <div className={`bg-retro-blue text-retro-white border-4 border-retro-white p-2 sm:p-4 lg:p-6 ${className}`}>
    {children}
  </div>
);

export default RetroBox;