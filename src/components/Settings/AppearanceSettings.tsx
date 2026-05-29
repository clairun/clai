import React, { useState } from 'react';
import { getThemePreference, setThemePreference, type ThemePreference } from '../../theme';
import styles from './AppearanceSettings.module.css';

const OPTIONS: { value: ThemePreference; label: string; desc: string }[] = [
  { value: 'light', label: 'Light', desc: 'Always use the light theme.' },
  { value: 'dark', label: 'Dark', desc: 'Always use the dark “Deep Space” theme.' },
  { value: 'system', label: 'System', desc: 'Follow your operating system setting.' },
];

const AppearanceSettings = () => {
  const [pref, setPref] = useState<ThemePreference>(getThemePreference());

  const choose = (next: ThemePreference) => {
    setThemePreference(next);
    setPref(next);
  };

  return (
    <div className={styles.section}>
      <h3 className={styles.heading}>Appearance</h3>
      <p className={styles.subtle}>Choose how clai looks. Applies instantly.</p>
      <div className={styles.options} role="radiogroup" aria-label="Theme">
        {OPTIONS.map((option) => (
          <button
            key={option.value}
            type="button"
            role="radio"
            aria-checked={pref === option.value}
            className={`${styles.option} ${pref === option.value ? styles.optionActive : ''}`}
            onClick={() => choose(option.value)}
          >
            <span className={styles.optionLabel}>{option.label}</span>
            <span className={styles.optionDesc}>{option.desc}</span>
          </button>
        ))}
      </div>
    </div>
  );
};

export default AppearanceSettings;
