/**
 * Skills, sources first. The overview lists each skill source with how much
 * of it is selected; opening one shows only that source's skills as a
 * compact grid, with the description of the skill under the pointer or
 * focus in a fixed strip below, so even the largest source fits without
 * scrolling. A search on the overview finds a skill without knowing its
 * source.
 *
 * Selection is an opaque set of ids: this says nothing about what selecting
 * a skill costs, which is the backend's business.
 */

import React, { useId, useMemo, useState } from 'react';
import { openGlobalSettings } from '../../utils/globalSettings';
import form from './SettingsForm.module.css';
import styles from './SkillPicker.module.css';

export interface SkillOption {
  id: string;
  name: string;
  description?: string | null;
  sourceId?: string | null;
  sourceName?: string | null;
}

export interface SkillGroup {
  key: string;
  name: string;
  skills: SkillOption[];
}

const UNGROUPED = '__ungrouped__';

/** Group by source; sources and the skills in each sorted by name. */
export const groupSkills = (skills: SkillOption[]): SkillGroup[] => {
  const groups = new Map<string, SkillGroup>();
  for (const skill of skills) {
    const key = skill.sourceId || UNGROUPED;
    let group = groups.get(key);
    if (!group) {
      group = { key, name: skill.sourceName || 'Other', skills: [] };
      groups.set(key, group);
    }
    group.skills.push(skill);
  }
  const sorted = [...groups.values()].sort((a, b) => a.name.localeCompare(b.name));
  for (const group of sorted) group.skills.sort((a, b) => a.name.localeCompare(b.name));
  return sorted;
};

const matches = (skill: SkillOption, query: string): boolean =>
  skill.name.toLowerCase().includes(query) || (skill.description || '').toLowerCase().includes(query);

const toggleId = (ids: string[], id: string, on: boolean): string[] => {
  if (on) return ids.includes(id) ? ids : [...ids, id];
  return ids.filter((other) => other !== id);
};

/** One skill as a checkbox tile; selected tiles are tinted and checked. */
const SkillTile = ({ skill, checked, onToggle, onFocusSkill, disabled, source }: {
  skill: SkillOption;
  checked: boolean;
  onToggle: (on: boolean) => void;
  onFocusSkill: (skill: SkillOption) => void;
  disabled?: boolean;
  /** Shown in search results, where tiles from several sources mix. */
  source?: string;
}) => (
  <li className={styles.tileItem}>
    <label
      className={`${styles.tile} ${checked ? styles.tileOn : ''}`}
      onMouseEnter={() => onFocusSkill(skill)}
      title={skill.description || undefined}
    >
      <input
        type="checkbox"
        checked={checked}
        onChange={(e) => onToggle(e.target.checked)}
        onFocus={() => onFocusSkill(skill)}
        disabled={disabled}
        aria-label={skill.name}
        aria-description={skill.description || undefined}
      />
      <span className={styles.tileName}>{skill.name}</span>
      {source && <span className={styles.tileSource}>{source}</span>}
    </label>
  </li>
);

/** The fixed strip that says what the hovered or focused skill does. */
const DescriptionStrip = ({ skill }: { skill: SkillOption | null }) => (
  <div className={styles.strip} aria-hidden="true">
    {skill ? (
      <>
        <span className={styles.stripName}>{skill.name}</span>
        <span className={styles.stripText}>{skill.description || 'No description.'}</span>
      </>
    ) : (
      <span className={styles.stripText}>Point at a skill or focus it to see what it does.</span>
    )}
  </div>
);

export const NoSkills = () => (
  <div className={styles.empty} role="status">
    <p className={form.lede}>No skills yet. Skills come from skill sources added in app settings.</p>
    <button type="button" className={form.secondaryButton} onClick={() => openGlobalSettings({ tab: 'skills' })}>
      Add a skill source
    </button>
  </div>
);

/**
 * The overview: one row per source with its selected count, or, while a
 * search is typed, every matching skill across sources.
 */
export const SkillSources = ({ groups, selectedIds, onChange, onOpen, disabled }: {
  groups: SkillGroup[];
  selectedIds: string[];
  onChange: (next: string[]) => void;
  onOpen: (key: string) => void;
  disabled?: boolean;
}) => {
  const [query, setQuery] = useState('');
  const [focused, setFocused] = useState<SkillOption | null>(null);
  const selected = useMemo(() => new Set(selectedIds), [selectedIds]);
  const trimmed = query.trim().toLowerCase();

  const results = useMemo(() => (
    trimmed
      ? groups.flatMap((group) => group.skills.filter((skill) => matches(skill, trimmed)).map((skill) => ({ skill, group })))
      : []
  ), [groups, trimmed]);

  return (
    <>
      <input
        type="search"
        className={`${form.input} ${styles.search}`}
        value={query}
        onChange={(e) => setQuery(e.target.value)}
        placeholder="Find a skill in any source…"
        aria-label="Search skills"
        disabled={disabled}
      />
      {!trimmed ? (
        <ul className={styles.sources} aria-label="Skill sources">
          {groups.map((group) => {
            const count = group.skills.filter((skill) => selected.has(skill.id)).length;
            const total = group.skills.length;
            return (
              <li key={group.key} className={styles.sourceItem}>
                <button
                  type="button"
                  data-source={group.key}
                  className={`${styles.source} ${count > 0 ? styles.sourceOn : ''}`}
                  onClick={() => onOpen(group.key)}
                  aria-label={`${group.name}, ${count} of ${total} selected`}
                >
                  <span className={styles.sourceName}>{group.name}</span>
                  <span className={styles.sourceCount}>
                    <strong>{count}</strong>/{total}
                  </span>
                  <span className={styles.meter} aria-hidden="true">
                    <span style={{ width: `${total ? (count / total) * 100 : 0}%` }} />
                  </span>
                  <span className={styles.chevron} aria-hidden="true">›</span>
                </button>
              </li>
            );
          })}
        </ul>
      ) : results.length === 0 ? (
        <p className={form.lede} role="status">No skills match “{query.trim()}”.</p>
      ) : (
        <>
          <ul className={`${styles.tiles} ${styles.results}`} aria-label="Matching skills">
            {results.map(({ skill, group }) => (
              <SkillTile
                key={skill.id}
                skill={skill}
                source={group.name}
                checked={selected.has(skill.id)}
                onToggle={(on) => onChange(toggleId(selectedIds, skill.id, on))}
                onFocusSkill={setFocused}
                disabled={disabled}
              />
            ))}
          </ul>
          <DescriptionStrip skill={focused} />
        </>
      )}
    </>
  );
};

/** One source, opened: its skills only, with Select all / Clear. */
export const SkillSourceView = ({ group, selectedIds, onChange, disabled, headingRef, onBack, context }: {
  group: SkillGroup;
  selectedIds: string[];
  onChange: (next: string[]) => void;
  disabled?: boolean;
  headingRef: React.Ref<HTMLHeadingElement>;
  onBack: () => void;
  /** What "back" returns to, for the crumb's name. */
  context: string;
}) => {
  const [filter, setFilter] = useState('');
  const [focused, setFocused] = useState<SkillOption | null>(null);
  const headingId = useId();
  const selected = useMemo(() => new Set(selectedIds), [selectedIds]);
  const trimmed = filter.trim().toLowerCase();
  const shown = trimmed ? group.skills.filter((skill) => matches(skill, trimmed)) : group.skills;
  const count = group.skills.filter((skill) => selected.has(skill.id)).length;
  const allOn = count === group.skills.length;

  const setAll = (on: boolean) => {
    const ids = group.skills.map((skill) => skill.id);
    if (on) onChange([...selectedIds, ...ids.filter((id) => !selected.has(id))]);
    else onChange(selectedIds.filter((id) => !ids.includes(id)));
  };

  return (
    <section className={`${form.panel} ${form.panelFill}`}>
      <header className={form.panelHeader}>
        <button type="button" className={form.quietButton} onClick={onBack} aria-label={`Back to ${context}`}>
          ‹ {context}
        </button>
        <h3 id={headingId} className={form.panelTitle} ref={headingRef} tabIndex={-1}>{group.name}</h3>
        <span className={form.panelMeta} aria-live="polite">{count} of {group.skills.length} selected</span>
        <span className={form.panelActions}>
          <input
            type="search"
            className={`${form.input} ${styles.filter}`}
            value={filter}
            onChange={(e) => setFilter(e.target.value)}
            placeholder="Filter…"
            aria-label={`Filter ${group.name}`}
            disabled={disabled}
          />
          <button type="button" className={form.quietButton} onClick={() => setAll(!allOn)} disabled={disabled}>
            {allOn ? 'Clear all' : 'Select all'}
          </button>
          {count > 0 && !allOn && (
            <button type="button" className={form.quietButton} onClick={() => setAll(false)} disabled={disabled}>
              Clear
            </button>
          )}
        </span>
      </header>
      <div className={`${form.panelBody} ${styles.sourceBody}`}>
        {shown.length === 0 ? (
          <p className={form.lede} role="status">No skills in {group.name} match “{filter.trim()}”.</p>
        ) : (
          <ul className={styles.tiles} aria-label={`Skills in ${group.name}`}>
            {shown.map((skill) => (
              <SkillTile
                key={skill.id}
                skill={skill}
                checked={selected.has(skill.id)}
                onToggle={(on) => onChange(toggleId(selectedIds, skill.id, on))}
                onFocusSkill={setFocused}
                disabled={disabled}
              />
            ))}
          </ul>
        )}
        <DescriptionStrip skill={focused} />
      </div>
    </section>
  );
};
