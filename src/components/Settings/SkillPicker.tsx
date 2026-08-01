import { useMemo, useState } from 'react';
import styles from './SkillPicker.module.css';

export interface SkillOption {
  id: string;
  name: string;
  description?: string | null;
  sourceId?: string | null;
  sourceName?: string | null;
}

interface SkillPickerProps {
  skills: SkillOption[];
  selectedIds: string[];
  onChange: (next: string[]) => void;
  disabled?: boolean;
  /** Accessible name for the whole control. */
  label?: string;
}

interface SkillGroup {
  key: string;
  name: string;
  skills: SkillOption[];
}

const UNGROUPED = '__ungrouped__';

const Chevron = ({ open }: { open: boolean }) => (
  <svg
    className={`${styles.chevron} ${open ? styles.chevronOpen : ''}`}
    width="12"
    height="12"
    viewBox="0 0 24 24"
    fill="none"
    stroke="currentColor"
    strokeWidth="2.5"
    strokeLinecap="round"
    strokeLinejoin="round"
    aria-hidden="true"
  >
    <polyline points="9 18 15 12 9 6" />
  </svg>
);

/** Group by source, sources alphabetical, skills alphabetical inside each. */
const groupSkills = (skills: SkillOption[]): SkillGroup[] => {
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
  for (const group of sorted) {
    group.skills.sort((a, b) => a.name.localeCompare(b.name));
  }
  return sorted;
};

/**
 * Grouped, searchable multi-select for skills.
 *
 * Selection is an opaque set of ids — this component says nothing about what
 * selecting a skill costs, because that is the backend's business and is
 * changing (progressive disclosure). Bulk selection is therefore a plain,
 * unguarded action.
 */
const SkillPicker = ({ skills, selectedIds, onChange, disabled, label = 'Skills' }: SkillPickerProps) => {
  const [query, setQuery] = useState('');
  const [collapsed, setCollapsed] = useState<Set<string>>(new Set());

  const selected = useMemo(() => new Set(selectedIds), [selectedIds]);

  const allGroups = useMemo(() => groupSkills(skills), [skills]);

  const trimmedQuery = query.trim().toLowerCase();
  const groups = useMemo(() => {
    if (!trimmedQuery) return allGroups;
    return allGroups
      .map((group) => ({
        ...group,
        skills: group.skills.filter(
          (skill) =>
            skill.name.toLowerCase().includes(trimmedQuery) ||
            (skill.description || '').toLowerCase().includes(trimmedQuery)
        ),
      }))
      .filter((group) => group.skills.length > 0);
  }, [allGroups, trimmedQuery]);

  const selectedSkills = useMemo(
    () => skills.filter((skill) => selected.has(skill.id)),
    [skills, selected]
  );

  const toggle = (id: string, next: boolean) => {
    if (next) {
      if (selected.has(id)) return;
      onChange([...selectedIds, id]);
    } else {
      onChange(selectedIds.filter((selectedId) => selectedId !== id));
    }
  };

  const setGroupSelection = (group: SkillGroup, next: boolean) => {
    const ids = group.skills.map((skill) => skill.id);
    if (next) {
      const missing = ids.filter((id) => !selected.has(id));
      if (missing.length === 0) return;
      onChange([...selectedIds, ...missing]);
    } else {
      const remove = new Set(ids);
      onChange(selectedIds.filter((id) => !remove.has(id)));
    }
  };

  const toggleCollapsed = (key: string) => {
    setCollapsed((current) => {
      const next = new Set(current);
      if (next.has(key)) next.delete(key);
      else next.add(key);
      return next;
    });
  };

  if (skills.length === 0) {
    return (
      <p className={styles.empty}>
        No skills available. Add a skill source in app settings.
      </p>
    );
  }

  return (
    <div className={styles.picker}>
      <div className={styles.toolbar}>
        <input
          type="search"
          className={styles.search}
          value={query}
          onChange={(event) => setQuery(event.target.value)}
          placeholder="Search skills…"
          aria-label={`Search ${label.toLowerCase()}`}
          disabled={disabled}
        />
        <span className={styles.tally}>
          {selectedSkills.length} of {skills.length} selected
        </span>
        {selectedSkills.length > 0 && (
          <button
            type="button"
            className={styles.linkButton}
            onClick={() => onChange([])}
            disabled={disabled}
          >
            Clear all
          </button>
        )}
      </div>

      {selectedSkills.length > 0 && (
        <ul className={styles.chipList} aria-label={`Selected ${label.toLowerCase()}`}>
          {selectedSkills.map((skill) => (
            <li key={skill.id} className={styles.chip}>
              {skill.name}
              <button
                type="button"
                className={styles.chipRemove}
                onClick={() => toggle(skill.id, false)}
                disabled={disabled}
                aria-label={`Remove ${skill.name}`}
              >
                ×
              </button>
            </li>
          ))}
        </ul>
      )}

      <div className={styles.groups}>
        {groups.length === 0 ? (
          <p className={styles.empty}>No skills match “{query.trim()}”.</p>
        ) : (
          groups.map((group) => {
            // While searching, keep every matching group open — collapsing a
            // group the user just searched into hides the answer.
            const open = Boolean(trimmedQuery) || !collapsed.has(group.key);
            const selectedInGroup = group.skills.filter((skill) => selected.has(skill.id)).length;
            const allSelected = selectedInGroup === group.skills.length;
            const bodyId = `skill-group-${group.key}`;
            return (
              <section key={group.key} className={styles.group}>
                <div className={styles.groupHeader}>
                  <button
                    type="button"
                    className={styles.groupToggle}
                    onClick={() => toggleCollapsed(group.key)}
                    aria-expanded={open}
                    aria-controls={bodyId}
                    disabled={Boolean(trimmedQuery)}
                  >
                    <Chevron open={open} />
                    <span className={styles.groupName}>{group.name}</span>
                    <span className={styles.groupCount}>
                      {selectedInGroup}/{group.skills.length}
                    </span>
                  </button>
                  <button
                    type="button"
                    className={styles.linkButton}
                    onClick={() => setGroupSelection(group, !allSelected)}
                    disabled={disabled}
                  >
                    {allSelected ? 'Clear' : 'Select all'}
                  </button>
                </div>
                {open && (
                  <ul id={bodyId} className={styles.skillList}>
                    {group.skills.map((skill) => (
                      <li key={skill.id}>
                        <label className={styles.skillRow}>
                          <input
                            type="checkbox"
                            checked={selected.has(skill.id)}
                            onChange={(event) => toggle(skill.id, event.target.checked)}
                            disabled={disabled}
                          />
                          <span className={styles.skillText}>
                            <span className={styles.skillName}>{skill.name}</span>
                            {skill.description && (
                              <span className={styles.skillDescription} title={skill.description}>
                                {skill.description}
                              </span>
                            )}
                          </span>
                        </label>
                      </li>
                    ))}
                  </ul>
                )}
              </section>
            );
          })
        )}
      </div>
    </div>
  );
};

export default SkillPicker;
