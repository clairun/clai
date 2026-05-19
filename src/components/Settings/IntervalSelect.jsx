/**
 * IntervalSelect Component
 *
 * Custom styled dropdown for selecting check intervals.
 */

import React, { useState, useRef, useEffect } from 'react';
import ReactDOM from 'react-dom';
import styles from './IntervalSelect.module.css';

/**
 * Chevron down icon
 */
const ChevronIcon = ({ isOpen }) => (
  <svg
    width="14"
    height="14"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="2"
    strokeLinecap="round"
    strokeLinejoin="round"
    className={`${styles.chevron} ${isOpen ? styles.chevronOpen : ''}`}
  >
    <polyline points="6 9 12 15 18 9" />
  </svg>
);

/**
 * Check icon for selected item
 */
const CheckIcon = () => (
  <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
    <polyline points="20 6 9 17 4 12" />
  </svg>
);

/**
 * Interval presets
 */
const INTERVAL_OPTIONS = [
  { value: 5, label: '5 minutes' },
  { value: 15, label: '15 minutes' },
  { value: 30, label: '30 minutes' },
  { value: 60, label: '1 hour' },
  { value: 120, label: '2 hours' },
  { value: 360, label: '6 hours' },
  { value: 720, label: '12 hours' },
  { value: 1440, label: '24 hours' },
];

/**
 * IntervalSelect - Custom dropdown for interval selection
 *
 * @param {Object} props
 * @param {number} props.value - Current selected value
 * @param {Function} props.onChange - Callback when value changes
 * @param {boolean} props.disabled - Whether the select is disabled
 * @param {string} props.id - ID for accessibility
 */
const IntervalSelect = ({ value, onChange, disabled, id }) => {
  const [isOpen, setIsOpen] = useState(false);
  const [dropdownStyle, setDropdownStyle] = useState({});
  const containerRef = useRef(null);
  const triggerRef = useRef(null);
  const dropdownRef = useRef(null);

  // Find the label for current value
  const selectedOption = value === 0
    ? { value: 0, label: 'On demand' }
    : INTERVAL_OPTIONS.find(opt => opt.value === value) || INTERVAL_OPTIONS[2];

  // Update dropdown position when opened
  useEffect(() => {
    if (isOpen && triggerRef.current) {
      const rect = triggerRef.current.getBoundingClientRect();
      const dropdownHeight = 280; // max-height of dropdown
      const spaceBelow = window.innerHeight - rect.bottom;
      const spaceAbove = rect.top;

      // Position above if not enough space below
      const showAbove = spaceBelow < dropdownHeight && spaceAbove > spaceBelow;

      setDropdownStyle({
        position: 'fixed',
        left: rect.left,
        width: rect.width,
        ...(showAbove
          ? { bottom: window.innerHeight - rect.top + 4 }
          : { top: rect.bottom + 4 }),
      });
    }
  }, [isOpen]);

  // Close dropdown when clicking outside
  useEffect(() => {
    const handleClickOutside = (event) => {
      const isOutsideContainer = containerRef.current && !containerRef.current.contains(event.target);
      const isOutsideDropdown = dropdownRef.current && !dropdownRef.current.contains(event.target);
      if (isOutsideContainer && isOutsideDropdown) {
        setIsOpen(false);
      }
    };

    if (isOpen) {
      document.addEventListener('mousedown', handleClickOutside);
    }

    return () => {
      document.removeEventListener('mousedown', handleClickOutside);
    };
  }, [isOpen]);

  // Handle keyboard navigation
  const handleKeyDown = (e) => {
    if (disabled) return;

    switch (e.key) {
      case 'Enter':
      case ' ':
        e.preventDefault();
        setIsOpen(!isOpen);
        break;
      case 'Escape':
        setIsOpen(false);
        break;
      case 'ArrowDown':
        e.preventDefault();
        if (!isOpen) {
          setIsOpen(true);
        } else {
          const currentIndex = INTERVAL_OPTIONS.findIndex(opt => opt.value === value);
          const nextIndex = Math.min(currentIndex + 1, INTERVAL_OPTIONS.length - 1);
          onChange(INTERVAL_OPTIONS[nextIndex].value);
        }
        break;
      case 'ArrowUp':
        e.preventDefault();
        if (isOpen) {
          const currentIndex = INTERVAL_OPTIONS.findIndex(opt => opt.value === value);
          const prevIndex = Math.max(currentIndex - 1, 0);
          onChange(INTERVAL_OPTIONS[prevIndex].value);
        }
        break;
      default:
        break;
    }
  };

  const handleOptionClick = (optionValue) => {
    onChange(optionValue);
    setIsOpen(false);
  };

  const toggleOpen = () => {
    if (!disabled) {
      setIsOpen(!isOpen);
    }
  };

  return (
    <div
      className={`${styles.container} ${disabled ? styles.disabled : ''}`}
      ref={containerRef}
    >
      <button
        type="button"
        ref={triggerRef}
        id={id}
        className={`${styles.trigger} ${isOpen ? styles.triggerOpen : ''}`}
        onClick={toggleOpen}
        onKeyDown={handleKeyDown}
        disabled={disabled}
        aria-haspopup="listbox"
        aria-expanded={isOpen}
      >
        <span className={styles.selectedLabel}>{selectedOption.label}</span>
        <ChevronIcon isOpen={isOpen} />
      </button>

      {isOpen && ReactDOM.createPortal(
        <div ref={dropdownRef} className={styles.dropdown} style={dropdownStyle} role="listbox">
          {INTERVAL_OPTIONS.map((option) => (
            <button
              key={option.value}
              type="button"
              className={`${styles.option} ${option.value === value ? styles.optionSelected : ''}`}
              onClick={() => handleOptionClick(option.value)}
              role="option"
              aria-selected={option.value === value}
            >
              <span>{option.label}</span>
              {option.value === value && (
                <span className={styles.checkIcon}>
                  <CheckIcon />
                </span>
              )}
            </button>
          ))}
        </div>,
        document.body
      )}
    </div>
  );
};

export default IntervalSelect;
