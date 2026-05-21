/**
 * Dashboard Component
 *
 * Displays charts for metrics in a responsive grid layout.
 * State is owned by this component and persisted to Zustand store
 * (backed by SQLite) using the commandId. The component registers
 * an API with the CommandRegistry so other commands can add/remove charts.
 *
 * Features:
 * - Owns its own state (elements array)
 * - Registers API via useCommandRegistration
 * - Persists to Zustand store via useCommandStateManager
 * - Supports time range selection
 * - Global filtering and grouping options
 * - Allows removing individual metrics or clearing all
 */

import React, { useState, useMemo, useCallback, useEffect, useRef } from 'react';
import { useCommandRegistration } from '../../hooks/useCommandRegistration';
import { useCommandStateManager } from '../../hooks/useWorkspaceSelectors';
import { useDebounce } from '../../hooks/useDebounce';
import { validateDashboardElement } from '../../utils/dashboardElementValidator';
import ContextChart from '../ChartsView/ContextChart';
import styles from './Dashboard.module.css';

// Time interval options
const TIME_INTERVALS = [
  { label: '5m', value: 5, unit: 'minutes' },
  { label: '15m', value: 15, unit: 'minutes' },
  { label: '30m', value: 30, unit: 'minutes' },
  { label: '1h', value: 1, unit: 'hours' },
  { label: '2h', value: 2, unit: 'hours' },
  { label: '6h', value: 6, unit: 'hours' },
  { label: '12h', value: 12, unit: 'hours' },
  { label: '24h', value: 24, unit: 'hours' },
  { label: '7d', value: 7, unit: 'days' },
];

/**
 * Get auto-refresh interval in milliseconds based on selected time range
 */
const getAutoRefreshInterval = (interval) => {
  const { value, unit } = interval;

  // For short time ranges, refresh more frequently
  if (unit === 'minutes') {
    if (value <= 5) return 5 * 1000;      // 5m range → 5s refresh
    if (value <= 15) return 10 * 1000;    // 15m range → 10s refresh
    return 15 * 1000;                      // 30m range → 15s refresh
  }

  if (unit === 'hours') {
    if (value <= 1) return 30 * 1000;     // 1h range → 30s refresh
    if (value <= 2) return 60 * 1000;     // 2h range → 1min refresh
    if (value <= 6) return 2 * 60 * 1000; // 6h range → 2min refresh
    return 5 * 60 * 1000;                  // 12-24h range → 5min refresh
  }

  // Days - refresh less frequently
  return 10 * 60 * 1000;                   // 7d range → 10min refresh
};

const INITIAL_VISIBLE_COUNT = 20;

/**
 * Calculate time range based on interval
 */
const calculateTimeRange = (interval) => {
  const now = new Date();
  const before = now.toISOString();

  let after;
  switch (interval.unit) {
    case 'minutes':
      after = new Date(now.getTime() - interval.value * 60 * 1000).toISOString();
      break;
    case 'hours':
      after = new Date(now.getTime() - interval.value * 60 * 60 * 1000).toISOString();
      break;
    case 'days':
      after = new Date(now.getTime() - interval.value * 24 * 60 * 60 * 1000).toISOString();
      break;
    default:
      after = new Date(now.getTime() - 60 * 60 * 1000).toISOString();
  }

  return {
    after,
    before,
    intervalCount: 60,
  };
};

// Memoized FilterChip component
const FilterChip = React.memo(({
  option,
  filterLabel,
  isChecked,
  onFilterChange
}) => {
  const handleChange = useCallback((e) => {
    onFilterChange(filterLabel, option.value, e.target.checked);
  }, [filterLabel, option.value, onFilterChange]);

  return (
    <label className={styles.globalFilterChip}>
      <input
        type="checkbox"
        checked={isChecked}
        onChange={handleChange}
        className={styles.globalFilterCheckbox}
      />
      <span className={styles.globalFilterChipLabel}>{option.displayName}</span>
    </label>
  );
});

// Filter modal content
const FilterModalContent = React.memo(({
  groupByOptions,
  filterOptions,
  globalGroupBy,
  globalFilterBy,
  onGroupByChange,
  onFilterChange,
  searchQuery,
  onSearchChange,
  expandedGroups,
  onToggleGroupExpansion,
  onClose
}) => {
  return (
    <div className={styles.filterModal}>
      <div className={styles.filterModalHeader}>
        <h3 className={styles.filterModalTitle}>Global Filters & Grouping</h3>
        <button className={styles.filterModalClose} onClick={onClose} title="Close">
          ×
        </button>
      </div>

      <div className={styles.filterModalBody}>
        {/* Search Input */}
        <div className={styles.globalFilterSearchContainer}>
          <input
            type="text"
            className={styles.globalFilterSearchInput}
            placeholder="Search filters..."
            value={searchQuery}
            onChange={(e) => onSearchChange(e.target.value)}
            autoComplete="off"
          />
          {searchQuery && (
            <button
              className={styles.globalFilterSearchClear}
              onClick={() => onSearchChange('')}
              title="Clear search"
            >
              ×
            </button>
          )}
        </div>

        {/* Group By Section */}
        {groupByOptions.length > 0 && (
          <div className={styles.globalFilterSection}>
            <div className={styles.globalFilterSectionTitle}>Group By</div>
            <div className={styles.globalFilterChips}>
              {groupByOptions.map(option => (
                <label key={option.label} className={styles.globalFilterChip}>
                  <input
                    type="checkbox"
                    checked={globalGroupBy.includes(option.label)}
                    onChange={(e) => onGroupByChange(option.label, e.target.checked)}
                    className={styles.globalFilterCheckbox}
                  />
                  <span className={styles.globalFilterChipLabel}>{option.displayName}</span>
                </label>
              ))}
            </div>
          </div>
        )}

        {/* Filter By Section */}
        {Object.keys(filterOptions).length > 0 && (
          <div className={styles.globalFilterSection}>
            <div className={styles.globalFilterSectionTitle}>Filter By</div>
            <div className={styles.globalFilterGroupsContainer}>
              {Object.entries(filterOptions).map(([filterLabel, options]) => {
                const isExpanded = expandedGroups[filterLabel] || searchQuery.trim() !== '';
                const hasMore = options.length > INITIAL_VISIBLE_COUNT;
                const visibleOptions = isExpanded ? options : options.slice(0, INITIAL_VISIBLE_COUNT);

                return (
                  <div key={filterLabel} className={styles.globalFilterGroup}>
                    <div className={styles.globalFilterGroupTitle}>{filterLabel}</div>
                    <div className={styles.globalFilterChipsScrollable}>
                      {visibleOptions.map(option => {
                        const isChecked = globalFilterBy[filterLabel]?.includes(option.value) || false;
                        return (
                          <FilterChip
                            key={option.value}
                            option={option}
                            filterLabel={filterLabel}
                            isChecked={isChecked}
                            onFilterChange={onFilterChange}
                          />
                        );
                      })}
                      {hasMore && !searchQuery.trim() && (
                        <button
                          className={styles.showMoreButton}
                          onClick={() => onToggleGroupExpansion(filterLabel)}
                        >
                          {isExpanded
                            ? '▲ Show Less'
                            : `▼ +${options.length - INITIAL_VISIBLE_COUNT} more`
                          }
                        </button>
                      )}
                    </div>
                  </div>
                );
              })}
            </div>
          </div>
        )}
      </div>
    </div>
  );
});

/**
 * Default dashboard state
 */
const DEFAULT_DASHBOARD_STATE = { elements: [], timeRange: '1h' };

/**
 * Generate a unique element ID
 */
const generateElementId = () =>
  `el_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;


/**
 * Dashboard Component
 */
const Dashboard = ({ command }) => {
  const commandId = command?.id;

  // Use Zustand store for persistent state (backed by SQLite)
  const [persistedState, updatePersistedState] = useCommandStateManager(commandId);

  // Local state derived from persisted state
  const [elements, setElements] = useState(() => persistedState?.elements ?? DEFAULT_DASHBOARD_STATE.elements);

  // Track if initialized (to prevent initial save)
  const isInitializedRef = useRef(false);

  // Sync from persisted state on mount (handles restore from SQLite)
  useEffect(() => {
    if (persistedState?.elements && !isInitializedRef.current) {
      setElements(persistedState.elements);
    }
    isInitializedRef.current = true;
  }, [persistedState?.elements]);

  // Process pending charts from command args (when dashboard is created with initial charts)
  // Uses deterministic IDs (based on position) to naturally dedupe on StrictMode remount or tile split
  useEffect(() => {
    const pendingCharts = command?.args?.pendingCharts;
    if (!pendingCharts || !Array.isArray(pendingCharts) || pendingCharts.length === 0) return;

    // Create elements with deterministic IDs based on position
    const newElements = pendingCharts.map((config, index) => {
      const element = {
        id: `pending_${commandId}_${index}`,
        type: 'context-chart',
        config,
      };
      // Validate before adding
      const validation = validateDashboardElement(element);
      if (!validation.valid) {
        console.error('[Dashboard] Invalid pending chart:', validation.error);
        return null;
      }
      return element;
    }).filter(Boolean);

    if (newElements.length > 0) {
      setElements(prev => {
        // Dedupe: only add elements that don't already exist
        const existingIds = new Set(prev.map(el => el.id));
        const toAdd = newElements.filter(el => !existingIds.has(el.id));
        return toAdd.length > 0 ? [...prev, ...toAdd] : prev;
      });
    }
    // Only run once on mount - command.args won't change

  }, []);

  // Find the initial time interval from persisted state
  const initialInterval = TIME_INTERVALS.find(
    i => i.label === (persistedState?.timeRange ?? DEFAULT_DASHBOARD_STATE.timeRange)
  ) || TIME_INTERVALS[3];

  // Selected time interval
  const [selectedInterval, setSelectedInterval] = useState(initialInterval);

  // Auto-refresh state
  const [autoRefreshEnabled, setAutoRefreshEnabled] = useState(false);
  const [lastRefresh, setLastRefresh] = useState(Date.now());

  // Global filter state
  const [globalGroupBy, setGlobalGroupBy] = useState([]);
  const [globalFilterBy, setGlobalFilterBy] = useState({});
  const [applyGlobally, setApplyGlobally] = useState(false);
  const [isFilterModalOpen, setIsFilterModalOpen] = useState(false);
  const [chartSummaries, setChartSummaries] = useState({});
  const [searchQuery, setSearchQuery] = useState('');
  const debouncedSearchQuery = useDebounce(searchQuery, 300);
  const [expandedGroups, setExpandedGroups] = useState({});

  // Generate unique instance ID for keys
  const instanceId = useRef(`dashboard_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`);

  // Save state to Zustand store when it changes (debounced)
  // Zustand handles further debounced persistence to SQLite
  useEffect(() => {
    if (!commandId || !isInitializedRef.current) return;

    const timeoutId = setTimeout(() => {
      updatePersistedState({ elements, timeRange: selectedInterval.label });
    }, 300);

    return () => clearTimeout(timeoutId);
  }, [commandId, elements, selectedInterval, updatePersistedState]);

  // Get valid dashboard elements
  const dashboardElements = useMemo(() => {
    return elements.filter(element => validateDashboardElement(element).valid);
  }, [elements]);

  // Register API with CommandRegistry
  useCommandRegistration(
    commandId,
    () => ({
      type: 'dashboard',

      /**
       * Add a chart to the dashboard
       * @param {object} config - Chart configuration
       * @returns {string} Generated element ID
       */
      addChart: (config) => {
        const elementId = generateElementId();
        const element = {
          id: elementId,
          type: 'context-chart',
          config,
        };

        // Validate before adding
        const validation = validateDashboardElement(element);
        if (!validation.valid) {
          console.error('[Dashboard] Invalid element:', validation.error);
          return null;
        }

        setElements((prev) => [...prev, element]);
        return elementId;
      },

      /**
       * Remove a chart from the dashboard
       * @param {string} chartId - Chart element ID
       * @returns {boolean} Success
       */
      removeChart: (chartId) => {
        setElements((prev) => prev.filter((el) => el.id !== chartId));
        return true;
      },

      /**
       * Get all charts
       * @returns {Array} Array of chart elements
       */
      getCharts: () => elements.filter((el) => el.type === 'context-chart'),

      /**
       * Get all charts with detailed config
       * @returns {Array} Array of chart elements with full config
       */
      getChartsDetailed: () => elements.filter((el) => el.type === 'context-chart'),

      /**
       * Check if an element exists in the dashboard
       * @param {string} elementId - Element ID to check
       * @returns {boolean} True if exists
       */
      hasElement: (elementId) => elements.some((el) => el.id === elementId),

      /**
       * Set the time range for all charts
       * @param {string} range - Time range label (e.g., '15m', '1h')
       * @returns {boolean} Success
       */
      setTimeRange: (range) => {
        const interval = TIME_INTERVALS.find((i) => i.label === range);
        if (interval) {
          setSelectedInterval(interval);
          return true;
        }
        return false;
      },

      /**
       * Clear all charts from the dashboard
       */
      clearCharts: () => {
        setElements([]);
      },
    }),
    [elements, setElements, selectedInterval, setSelectedInterval]
  );

  // Calculate time range based on selected interval (recalculates on refresh)
  const timeRange = useMemo(() => {
    return calculateTimeRange(selectedInterval);

  }, [selectedInterval, lastRefresh]);

  // Handle time interval change
  const handleIntervalChange = useCallback((interval) => {
    setSelectedInterval(interval);
  }, []);

  // Handle manual refresh
  const handleRefreshNow = useCallback(() => {
    setLastRefresh(Date.now());
  }, []);

  // Toggle auto-refresh
  const handleToggleAutoRefresh = useCallback(() => {
    setAutoRefreshEnabled(prev => !prev);
  }, []);

  // Auto-refresh effect
  useEffect(() => {
    if (!autoRefreshEnabled || dashboardElements.length === 0) {
      return;
    }

    const intervalMs = getAutoRefreshInterval(selectedInterval);
    const timerId = setInterval(() => {
      setLastRefresh(Date.now());
    }, intervalMs);

    return () => clearInterval(timerId);
  }, [autoRefreshEnabled, selectedInterval, dashboardElements.length]);

  // Handle remove element
  const handleRemoveElement = useCallback((elementId) => {
    setElements((prev) => prev.filter((el) => el.id !== elementId));
  }, []);

  // Handle clear all
  const handleClearAll = useCallback(() => {
    setElements([]);
  }, []);

  // Handle chart summary updates
  const handleChartSummary = useCallback((context, summary) => {
    setChartSummaries(prev => ({
      ...prev,
      [context]: summary
    }));
  }, []);

  // Aggregate filter options from all chart summaries
  const chartSummariesHash = useMemo(() =>
    JSON.stringify(Object.keys(chartSummaries).sort()),
    [chartSummaries]
  );

  const aggregatedOptions = useMemo(() => {
    const groupByOptionsMap = new Map();
    const filterOptionsMap = new Map();

    Object.values(chartSummaries).forEach(summary => {
      if (!summary) return;

      if (summary.nodes && summary.nodes.length > 1) {
        groupByOptionsMap.set('node', { label: 'node', displayName: 'Node' });
        if (!filterOptionsMap.has('node')) {
          filterOptionsMap.set('node', new Map());
        }
        const nodeOptions = filterOptionsMap.get('node');
        summary.nodes.forEach(n => {
          nodeOptions.set(n.mg, { value: n.mg, displayName: n.nm || n.mg });
        });
      }

      if (summary.dimensions && summary.dimensions.length > 1) {
        groupByOptionsMap.set('dimension', { label: 'dimension', displayName: 'Dimension' });
        if (!filterOptionsMap.has('dimension')) {
          filterOptionsMap.set('dimension', new Map());
        }
        const dimOptions = filterOptionsMap.get('dimension');
        summary.dimensions.forEach(d => {
          dimOptions.set(d.id, { value: d.id, displayName: d.id });
        });
      }

      if (summary.instances && summary.instances.length > 1) {
        groupByOptionsMap.set('instance', { label: 'instance', displayName: 'Instance' });
        if (!filterOptionsMap.has('instance')) {
          filterOptionsMap.set('instance', new Map());
        }
        const instOptions = filterOptionsMap.get('instance');
        summary.instances.forEach(i => {
          instOptions.set(i.id, { value: i.id, displayName: i.id });
        });
      }

      if (summary.labels && Array.isArray(summary.labels)) {
        summary.labels.forEach(labelObj => {
          if (labelObj.vl && labelObj.vl.length > 1) {
            groupByOptionsMap.set(labelObj.id, { label: labelObj.id, displayName: labelObj.id });
            if (!filterOptionsMap.has(labelObj.id)) {
              filterOptionsMap.set(labelObj.id, new Map());
            }
            const labelOptions = filterOptionsMap.get(labelObj.id);
            labelObj.vl.forEach(v => {
              labelOptions.set(v.id, { value: v.id, displayName: v.id });
            });
          }
        });
      }
    });

    const groupByOptions = Array.from(groupByOptionsMap.values()).sort((a, b) =>
      a.displayName.localeCompare(b.displayName)
    );

    const filterOptions = {};
    filterOptionsMap.forEach((optionsMap, key) => {
      filterOptions[key] = Array.from(optionsMap.values()).sort((a, b) =>
        a.displayName.localeCompare(b.displayName)
      );
    });

    return { groupByOptions, filterOptions };
  }, [chartSummariesHash]);

  // Filter options based on search query
  const filteredOptions = useMemo(() => {
    if (!debouncedSearchQuery.trim()) {
      return aggregatedOptions;
    }

    const query = debouncedSearchQuery.toLowerCase();

    const filteredGroupByOptions = aggregatedOptions.groupByOptions.filter(option =>
      option.displayName.toLowerCase().includes(query)
    );

    const filteredFilterOptions = {};
    Object.entries(aggregatedOptions.filterOptions).forEach(([filterLabel, options]) => {
      const filteredOpts = options.filter(option =>
        option.displayName.toLowerCase().includes(query) ||
        filterLabel.toLowerCase().includes(query)
      );
      if (filteredOpts.length > 0) {
        filteredFilterOptions[filterLabel] = filteredOpts;
      }
    });

    return {
      groupByOptions: filteredGroupByOptions,
      filterOptions: filteredFilterOptions
    };
  }, [aggregatedOptions, debouncedSearchQuery]);

  // Filter handlers
  const handleSearchChange = useCallback((value) => {
    setSearchQuery(value);
  }, []);

  const handleToggleGroupExpansion = useCallback((groupKey) => {
    setExpandedGroups(prev => ({
      ...prev,
      [groupKey]: !prev[groupKey]
    }));
  }, []);

  const handleGlobalGroupByChange = useCallback((label, isChecked) => {
    setGlobalGroupBy(prev => {
      if (isChecked) {
        return [...prev, label];
      } else {
        return prev.filter(l => l !== label);
      }
    });
  }, []);

  const handleGlobalFilterChange = useCallback((filterLabel, value, isChecked) => {
    setGlobalFilterBy(prev => {
      const updated = { ...prev };
      if (isChecked) {
        if (!updated[filterLabel]) {
          updated[filterLabel] = [];
        }
        updated[filterLabel] = [...updated[filterLabel], value];
      } else {
        if (updated[filterLabel]) {
          updated[filterLabel] = updated[filterLabel].filter(v => v !== value);
          if (updated[filterLabel].length === 0) {
            delete updated[filterLabel];
          }
        }
      }
      return updated;
    });
  }, []);

  const handleRemoveGlobalGroupBy = useCallback((label) => {
    setGlobalGroupBy(prev => prev.filter(l => l !== label));
  }, []);

  const handleRemoveGlobalFilter = useCallback((filterLabel, value) => {
    setGlobalFilterBy(prev => {
      const updated = { ...prev };
      if (updated[filterLabel]) {
        updated[filterLabel] = updated[filterLabel].filter(v => v !== value);
        if (updated[filterLabel].length === 0) {
          delete updated[filterLabel];
        }
      }
      return updated;
    });
  }, []);

  const handleClearGlobalFilters = useCallback(() => {
    setGlobalGroupBy([]);
    setGlobalFilterBy({});
  }, []);

  const handleToggleApplyGlobally = useCallback(() => {
    setApplyGlobally(prev => !prev);
  }, []);

  const getFilterDisplayName = useCallback((filterLabel, value) => {
    const options = aggregatedOptions.filterOptions[filterLabel];
    if (!options) return value;
    const option = options.find(opt => opt.value === value);
    return option ? option.displayName : value;
  }, [aggregatedOptions]);

  const hasGlobalFilters = globalGroupBy.length > 0 || Object.keys(globalFilterBy).length > 0;

  const memoizedGlobalFilterBy = useMemo(() => globalFilterBy, [
    JSON.stringify(globalFilterBy)
  ]);

  const memoizedGlobalGroupBy = useMemo(() => globalGroupBy, [
    JSON.stringify(globalGroupBy)
  ]);

  // Empty state
  if (dashboardElements.length === 0) {
    return (
      <div className={styles.dashboardWrapper}>
        <div className={styles.headerBar}>
          <div className={styles.headerLeft}>
            <span className={styles.headerTitle}>Dashboard</span>
          </div>
        </div>
        <div className={styles.dashboardContainer}>
          <div className={styles.emptyState}>
            <div className={styles.emptyStateIcon}>📊</div>
            <h3 className={styles.emptyStateTitle}>No metrics in dashboard</h3>
            <p className={styles.emptyStateText}>
              Use the assistant to add charts here from your available tools.
              <br />
              Charts will appear automatically when you select metrics.
            </p>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className={styles.dashboardWrapper}>
      {/* Header Bar */}
      <div className={styles.headerBar}>
        <div className={styles.headerLeft}>
          <span className={styles.headerTitle}>Dashboard</span>
          <span className={styles.metricCount}>{dashboardElements.length} element{dashboardElements.length !== 1 ? 's' : ''}</span>
          <div className={styles.timeIntervalSelector}>
            {TIME_INTERVALS.map((interval) => (
              <button
                key={interval.label}
                className={`${styles.intervalButton} ${selectedInterval.label === interval.label ? styles.active : ''}`}
                onClick={() => handleIntervalChange(interval)}
              >
                {interval.label}
              </button>
            ))}
          </div>
        </div>
        <div className={styles.headerRight}>
          {/* Refresh Now Button */}
          <button
            className={styles.refreshButton}
            onClick={handleRefreshNow}
            title="Refresh now"
          >
            <svg width="16" height="16" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round">
              <path d="M1 8a7 7 0 0 1 7-7 7 7 0 0 1 6.3 4" />
              <path d="M15 8a7 7 0 0 1-7 7 7 7 0 0 1-6.3-4" />
              <path d="M14.3 1v4h-4" />
              <path d="M1.7 15v-4h4" />
            </svg>
          </button>

          {/* Auto-refresh Toggle */}
          <div className={styles.compactToggleContainer}>
            <span className={styles.compactToggleLabel}>Auto</span>
            <button
              className={`${styles.compactToggleSwitch} ${autoRefreshEnabled ? styles.active : ''}`}
              onClick={handleToggleAutoRefresh}
              role="switch"
              aria-checked={autoRefreshEnabled}
              title={autoRefreshEnabled ? `Auto-refresh every ${Math.round(getAutoRefreshInterval(selectedInterval) / 1000)}s` : 'Enable auto-refresh'}
            >
              <span className={styles.compactToggleSlider} />
            </button>
          </div>

          {/* Filter Toggle Button */}
          <button
            className={`${styles.filterToggleButton} ${applyGlobally ? styles.active : ''}`}
            onClick={() => setIsFilterModalOpen(true)}
            title="Open filters"
          >
            <svg width="16" height="16" viewBox="0 0 16 16" fill="currentColor">
              <path d="M1.5 3h13M3.5 6h9M5.5 9h5M7 12h2" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" />
            </svg>
            Filters
            {hasGlobalFilters && (
              <span className={styles.filterBadge}>
                {globalGroupBy.length + Object.values(globalFilterBy).reduce((sum, arr) => sum + arr.length, 0)}
              </span>
            )}
          </button>

          {/* Global Toggle */}
          <div className={styles.compactToggleContainer}>
            <span className={styles.compactToggleLabel}>Apply to all</span>
            <button
              className={`${styles.compactToggleSwitch} ${applyGlobally ? styles.active : ''}`}
              onClick={handleToggleApplyGlobally}
              role="switch"
              aria-checked={applyGlobally}
            >
              <span className={styles.compactToggleSlider} />
            </button>
          </div>

          <button className={styles.clearButton} onClick={handleClearAll}>
            Clear All
          </button>
        </div>
      </div>

      {/* Active Filters Bar */}
      {hasGlobalFilters && applyGlobally && (
        <div className={styles.activeFiltersBar}>
          <div className={styles.activeFiltersList}>
            {globalGroupBy.map(group => (
              <span key={group} className={styles.activeFilterTag}>
                <span className={styles.activeFilterPrefix}>Group:</span>
                <span className={styles.activeFilterValue}>{group}</span>
                <button
                  className={styles.activeFilterRemove}
                  onClick={() => handleRemoveGlobalGroupBy(group)}
                  title={`Remove ${group} grouping`}
                >
                  ×
                </button>
              </span>
            ))}

            {Object.entries(globalFilterBy).map(([filterLabel, values]) =>
              values.map(value => (
                <span key={`${filterLabel}-${value}`} className={styles.activeFilterTag}>
                  <span className={styles.activeFilterPrefix}>{filterLabel}:</span>
                  <span className={styles.activeFilterValue}>
                    {getFilterDisplayName(filterLabel, value)}
                  </span>
                  <button
                    className={styles.activeFilterRemove}
                    onClick={() => handleRemoveGlobalFilter(filterLabel, value)}
                    title={`Remove ${filterLabel} filter`}
                  >
                    ×
                  </button>
                </span>
              ))
            )}
          </div>
          <button className={styles.clearFiltersButton} onClick={handleClearGlobalFilters}>
            Clear all filters
          </button>
        </div>
      )}

      {/* Charts Grid */}
      <div className={styles.dashboardContainer}>
        <div className={styles.chartsGrid}>
          {dashboardElements.map((element) => {
            // Currently only supporting context-chart type
            if (element.type === 'context-chart') {
              const { context, groupBy, filterBy, valueAgg, timeAgg, spaceId, roomId } = element.config;
              return (
                <div key={`${instanceId.current}-${element.id}`} className={styles.chartCard}>
                  <ContextChart
                    context={context}
                    groupBy={applyGlobally ? memoizedGlobalGroupBy : (groupBy || [])}
                    filterBy={applyGlobally ? memoizedGlobalFilterBy : (filterBy || {})}
                    valueAgg={valueAgg || 'avg'}
                    timeAgg={timeAgg || 'average'}
                    after={timeRange.after}
                    before={timeRange.before}
                    intervalCount={timeRange.intervalCount}
                    space={spaceId ? { id: spaceId } : null}
                    room={roomId ? { id: roomId } : null}
                    onRemove={() => handleRemoveElement(element.id)}
                    onSummaryUpdate={handleChartSummary}
                    showRefreshIndicator={false}
                  />
                </div>
              );
            }
            // Unsupported element type
            return null;
          })}
        </div>
      </div>

      {/* Filter Modal */}
      {isFilterModalOpen && (
        <div className={styles.filterModalOverlay} onClick={() => setIsFilterModalOpen(false)}>
          <div className={styles.filterModalContainer} onClick={(e) => e.stopPropagation()}>
            <FilterModalContent
              groupByOptions={filteredOptions.groupByOptions}
              filterOptions={filteredOptions.filterOptions}
              globalGroupBy={globalGroupBy}
              globalFilterBy={globalFilterBy}
              onGroupByChange={handleGlobalGroupByChange}
              onFilterChange={handleGlobalFilterChange}
              searchQuery={searchQuery}
              onSearchChange={handleSearchChange}
              expandedGroups={expandedGroups}
              onToggleGroupExpansion={handleToggleGroupExpansion}
              onClose={() => setIsFilterModalOpen(false)}
            />
          </div>
        </div>
      )}
    </div>
  );
};

export default Dashboard;
