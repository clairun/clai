import React, { useRef, useEffect, useState, useCallback, useMemo } from 'react';
import ReactDOM from 'react-dom';
import * as d3 from 'd3';
import { getData } from '../../api/client';
import NetdataSpinner from '../common/NetdataSpinner';
import styles from './ContextChart.module.css';

const normalizeFilterMap = (filterMap) => {
  if (!filterMap || typeof filterMap !== 'object') return {};

  return Object.fromEntries(
    Object.entries(filterMap)
      .filter(([key]) => !!key)
      .map(([key, value]) => [key, Array.isArray(value) ? value : [value].filter(Boolean)])
  );
};

// Memoized FilterChip component to prevent unnecessary re-renders
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
    <label className={styles.filterChip}>
      <input
        type="checkbox"
        checked={isChecked}
        onChange={handleChange}
        className={styles.filterCheckbox}
      />
      <span className={styles.filterChipLabel}>{option.displayName}</span>
    </label>
  );
});

// Memoized FilterPanelContent component for better performance
const ChartFilterPanelContent = React.memo(({
  groupByOptions,
  filterOptions,
  activeGroupBy,
  activeFilters,
  onGroupByChange,
  onFilterChange
}) => {
  return (
    <>
      {/* Group By Section */}
      {groupByOptions.length > 0 && (
        <div className={styles.filterSection}>
          <div className={styles.filterSectionTitle}>Group By</div>
          <div className={styles.filterChips}>
            {groupByOptions.map(option => (
              <label key={option.label} className={styles.filterChip}>
                <input
                  type="checkbox"
                  checked={activeGroupBy.includes(option.label)}
                  onChange={(e) => onGroupByChange(option.label, e.target.checked)}
                  className={styles.filterCheckbox}
                />
                <span className={styles.filterChipLabel}>{option.displayName}</span>
              </label>
            ))}
          </div>
        </div>
      )}

      {/* Filter By Section */}
      {Object.keys(filterOptions).length > 0 && (
        <div className={styles.filterSection}>
          <div className={styles.filterSectionTitle}>Filter By</div>
          <div className={styles.filterGroupsContainer}>
            {Object.entries(filterOptions).map(([filterLabel, options]) => (
              <div key={filterLabel} className={styles.filterGroup}>
                <div className={styles.filterGroupTitle}>{filterLabel}</div>
                <div className={styles.filterChipsScrollable}>
                  {options.map(option => {
                    const isChecked = activeFilters[filterLabel]?.includes(option.value) || false;
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
                </div>
              </div>
            ))}
          </div>
        </div>
      )}
    </>
  );
});

/**
 * Downsample data to reduce points for performance
 */
const downsampleData = (data, targetPoints = 500) => {
  if (!data || data.length <= targetPoints) return data;

  const step = Math.ceil(data.length / targetPoints);
  return data.filter((_, i) => i % step === 0);
};

/**
 * ContextChart Component - Ultra-Optimized Canvas Version
 * Minimal SVG overlay with single-circle interaction
 */
const ContextChart = ({
  context,
  groupBy = [],
  filterBy = [],
  valueAgg = 'avg',
  timeAgg = 'average',
  after,
  before,
  intervalCount = 15,
  space,
  room,
  onRemove,
  onSummaryUpdate,
  showRefreshIndicator = true,
  zoom = 1, // React Flow zoom level for crisp rendering at any zoom
}) => {

  const canvasRef = useRef(null);
  const svgRef = useRef(null);
  const containerRef = useRef(null);
  const tooltipRef = useRef(null);
  const [dimensions, setDimensions] = useState({ width: 0, height: 0 });
  const [error, setError] = useState(null);
  const [loading, setLoading] = useState(true);
  const [chartData, setChartData] = useState(null);
  const [summary, setSummary] = useState(null);
  const [tooltip, setTooltip] = useState({
    visible: false,
    x: 0,
    y: 0,
    content: null,
  });
  const [selectedSeries, setSelectedSeries] = useState(null);

  // Track the computed interval count to avoid excessive refetching
  const [computedIntervalCount, setComputedIntervalCount] = useState(intervalCount);

  // Filter and grouping state
  const [isFilterPanelOpen, setIsFilterPanelOpen] = useState(false);
  const [activeGroupBy, setActiveGroupBy] = useState([]);
  const [activeFilters, setActiveFilters] = useState({});
  const [isInitialized, setIsInitialized] = useState(false);

  // Netdata chart color palette
  const DEFAULT_COLORS = useMemo(() => [
    '#00AB44',  // Netdata Green
    '#00B5D8',  // Netdata Teal
    '#3498DB',  // Sky Blue
    '#9B59B6',  // Purple
    '#F39C12',  // Orange
    '#E74C3C',  // Red
    '#1ABC9C',  // Turquoise
    '#34495E',  // Dark Gray
  ], []);

  // Convert filterBy prop to object format if it's an array
  const normalizeFilterBy = useCallback((filterByProp) => {
    if (!filterByProp) return {};
    if (!Array.isArray(filterByProp)) return normalizeFilterMap(filterByProp);

    const filters = {};
    filterByProp.forEach(filter => {
      if (!filters[filter.label]) {
        filters[filter.label] = [];
      }
      filters[filter.label].push(filter.value);
    });
    return filters;
  }, []);

  // Memoize normalized props
  const normalizedGroupBy = useMemo(() => {
    return groupBy && Array.isArray(groupBy) ? groupBy : [];
  }, [JSON.stringify(groupBy)]);

  const normalizedFilterBy = useMemo(() => {
    return normalizeFilterBy(filterBy);
  }, [normalizeFilterBy, JSON.stringify(filterBy)]);

  // Initialize active filters and groups from props - ONLY ON MOUNT
  useEffect(() => {
    setActiveGroupBy(normalizedGroupBy);
    setActiveFilters(normalizedFilterBy);
    setIsInitialized(true);

  }, []);

  // Update filters/groups when props change
  useEffect(() => {
    if (!isInitialized) return;
    setActiveGroupBy(normalizedGroupBy);
    setActiveFilters(normalizedFilterBy);
  }, [normalizedGroupBy, normalizedFilterBy, isInitialized]);

  // Calculate dynamic interval count based on chart width
  // Formula: chart_region_width_px / 10, with reasonable min/max bounds
  const dynamicIntervalCount = useMemo(() => {
    if (dimensions.width === 0) return intervalCount;

    const margin = { left: 70, right: 80 };
    const chartRegionWidth = dimensions.width - margin.left - margin.right;

    // Calculate points: 1 point per 10 pixels
    const calculatedPoints = Math.floor(chartRegionWidth / 10);

    // Apply reasonable bounds: min 10, max 200
    const boundedPoints = Math.max(10, Math.min(200, calculatedPoints));

    return boundedPoints;
  }, [dimensions.width, intervalCount]);

  // Update computed interval count with debouncing to prevent excessive API calls
  useEffect(() => {
    const timeoutId = setTimeout(() => {
      if (dynamicIntervalCount !== computedIntervalCount) {
        setComputedIntervalCount(dynamicIntervalCount);
      }
    }, 500); // 500ms debounce

    return () => clearTimeout(timeoutId);
  }, [dynamicIntervalCount, computedIntervalCount]);

  // Handle container resizing
  useEffect(() => {
    if (!containerRef.current) return;

    const resizeObserver = new ResizeObserver((entries) => {
      for (let entry of entries) {
        const width = entry.target.clientWidth;
        const height = Math.min(400, Math.max(250, width * 0.5));
        if (width > 0 && height > 0) {
          setDimensions({ width, height });
        }
      }
    });

    resizeObserver.observe(containerRef.current);

    const width = containerRef.current.clientWidth;
    if (width > 0) {
      const height = Math.min(400, Math.max(250, width * 0.5));
      setDimensions({ width, height });
    }

    return () => resizeObserver.disconnect();
  }, []);

  // Filter out groupBy/filterBy that don't exist in this chart's summary
  const getApplicableFilters = useCallback((groupByParam, filters, summaryData) => {
    if (!summaryData) {
      return { applicableGroupBy: groupByParam, applicableFilters: filters, skipped: { groupBy: [], filters: {} } };
    }

    const availableLabels = new Set(['node', 'dimension', 'instance']);
    const skippedGroupBy = [];
    const skippedFilters = {};

    if (summaryData.labels && Array.isArray(summaryData.labels)) {
      summaryData.labels.forEach(labelObj => {
        if (labelObj.id) {
          availableLabels.add(labelObj.id);
        }
      });
    }

    const applicableGroupBy = groupByParam.filter(label => {
      const isAvailable = availableLabels.has(label);
      if (!isAvailable) {
        skippedGroupBy.push(label);
      }
      return isAvailable;
    });

    const applicableFilters = {};
    Object.entries(normalizeFilterMap(filters)).forEach(([filterLabel, values]) => {
      if (!availableLabels.has(filterLabel)) {
        skippedFilters[filterLabel] = values;
        return;
      }

      if (filterLabel === 'node' && summaryData.nodes) {
        const availableNodeIds = new Set(summaryData.nodes.map(n => n.mg));
        const applicableValues = values.filter(v => availableNodeIds.has(v));
        const skippedValues = values.filter(v => !availableNodeIds.has(v));

        if (applicableValues.length > 0) {
          applicableFilters[filterLabel] = applicableValues;
        }
        if (skippedValues.length > 0) {
          skippedFilters[filterLabel] = skippedValues;
        }
      } else if (filterLabel === 'dimension' && summaryData.dimensions) {
        const availableDimIds = new Set(summaryData.dimensions.map(d => d.id));
        const applicableValues = values.filter(v => availableDimIds.has(v));
        const skippedValues = values.filter(v => !availableDimIds.has(v));

        if (applicableValues.length > 0) {
          applicableFilters[filterLabel] = applicableValues;
        }
        if (skippedValues.length > 0) {
          skippedFilters[filterLabel] = skippedValues;
        }
      } else if (filterLabel === 'instance' && summaryData.instances) {
        const availableInstIds = new Set(summaryData.instances.map(i => i.id));
        const applicableValues = values.filter(v => availableInstIds.has(v));
        const skippedValues = values.filter(v => !availableInstIds.has(v));

        if (applicableValues.length > 0) {
          applicableFilters[filterLabel] = applicableValues;
        }
        if (skippedValues.length > 0) {
          skippedFilters[filterLabel] = skippedValues;
        }
      } else {
        const labelObj = summaryData.labels?.find(l => l.id === filterLabel);
        if (labelObj && labelObj.vl) {
          const availableValues = new Set(labelObj.vl.map(v => v.id));
          const applicableValues = values.filter(v => availableValues.has(v));
          const skippedValues = values.filter(v => !availableValues.has(v));

          if (applicableValues.length > 0) {
            applicableFilters[filterLabel] = applicableValues;
          }
          if (skippedValues.length > 0) {
            skippedFilters[filterLabel] = skippedValues;
          }
        } else {
          skippedFilters[filterLabel] = values;
        }
      }
    });

    return {
      applicableGroupBy,
      applicableFilters,
      skipped: { groupBy: skippedGroupBy, filters: skippedFilters }
    };
  }, []);

  const buildGetDataParams = useCallback((groupByParam, filters) => {
    const nodeIDs = [];
    const dimensions = [];
    const instances = [];
    const labels = [];
    const normalizedFilters = normalizeFilterMap(filters);

    Object.keys(normalizedFilters).forEach(label => {
      const values = normalizedFilters[label];
      values.forEach(value => {
        switch (label) {
          case 'node':
            nodeIDs.push(value);
            break;
          case 'dimension':
            dimensions.push(value);
            break;
          case 'instance':
            instances.push(value);
            break;
          default:
            labels.push(`${label}:${value}`);
            break;
        }
      });
    });

    const systemLabels = ['node', 'dimension', 'instance'];
    const groupedBy = [];
    const groupedByLabel = [];

    groupByParam.forEach(label => {
      if (systemLabels.includes(label)) {
        groupedBy.push(label);
      } else {
        if (!groupedBy.includes('label')) {
          groupedBy.push('label');
        }
        groupedByLabel.push(label);
      }
    });

    if (groupedBy.length === 0) {
      groupedBy.push('dimension');
    }

    const afterTimestamp = Math.floor(new Date(after).getTime() / 1000);
    const beforeTimestamp = Math.floor(new Date(before).getTime() / 1000);

    return {
      scope: {
        contexts: [context],
        nodes: nodeIDs.length > 0 ? nodeIDs : []
      },
      selectors: {
        dimensions: dimensions.length > 0 ? dimensions : ['*'],
        instances: instances.length > 0 ? instances : ['*'],
        labels: labels.length > 0 ? labels : ['*']
      },
      aggregations: {
        metrics: [
          {
            group_by: groupedBy,
            group_by_label: groupedByLabel,
            aggregation: valueAgg
          }
        ],
        time: {
          time_group: timeAgg,
          time_resampling: 0
        }
      },
      window: {
        after: afterTimestamp,
        before: beforeTimestamp,
        points: computedIntervalCount
      }
    };
  }, [context, valueAgg, timeAgg, after, before, computedIntervalCount]);

  // Deduplicate summary data
  const deduplicateSummary = useCallback((summaryData) => {
    if (!summaryData) return summaryData;

    const deduplicated = { ...summaryData };

    if (summaryData.nodes && Array.isArray(summaryData.nodes)) {
      const seenNodeIds = new Map();
      const uniqueNodes = [];

      summaryData.nodes.forEach(node => {
        if (node.mg && !seenNodeIds.has(node.mg)) {
          seenNodeIds.set(node.mg, true);
          uniqueNodes.push(node);
        }
      });

      deduplicated.nodes = uniqueNodes;
    }

    if (summaryData.dimensions && Array.isArray(summaryData.dimensions)) {
      const seenDimIds = new Map();
      const uniqueDimensions = [];

      summaryData.dimensions.forEach(dim => {
        if (dim.id && !seenDimIds.has(dim.id)) {
          seenDimIds.set(dim.id, true);
          uniqueDimensions.push(dim);
        }
      });

      deduplicated.dimensions = uniqueDimensions;
    }

    if (summaryData.instances && Array.isArray(summaryData.instances)) {
      const seenInstIds = new Map();
      const uniqueInstances = [];

      summaryData.instances.forEach(inst => {
        if (inst.id && !seenInstIds.has(inst.id)) {
          seenInstIds.set(inst.id, true);
          uniqueInstances.push(inst);
        }
      });

      deduplicated.instances = uniqueInstances;
    }

    if (summaryData.labels && Array.isArray(summaryData.labels)) {
      const seenLabelIds = new Map();
      const uniqueLabels = [];

      summaryData.labels.forEach(label => {
        if (label.id && !seenLabelIds.has(label.id)) {
          seenLabelIds.set(label.id, true);

          if (label.vl && Array.isArray(label.vl)) {
            const seenValueIds = new Map();
            const uniqueValues = [];

            label.vl.forEach(val => {
              if (val.id && !seenValueIds.has(val.id)) {
                seenValueIds.set(val.id, true);
                uniqueValues.push(val);
              }
            });

            uniqueLabels.push({ ...label, vl: uniqueValues });
          } else {
            uniqueLabels.push(label);
          }
        }
      });

      deduplicated.labels = uniqueLabels;
    }

    return deduplicated;
  }, []);

  const buildNodeMapping = (summaryData) => {
    const nodeMap = new Map();
    if (summaryData?.nodes && Array.isArray(summaryData.nodes)) {
      summaryData.nodes.forEach(node => {
        if (node.mg && node.nm) {
          if (!nodeMap.has(node.mg)) {
            nodeMap.set(node.mg, node.nm);
          }
        }
      });
    }
    return nodeMap;
  };

  const replaceNodeIdsInLabel = (label, nodeMap, isGroupedByNode) => {
    if (!isGroupedByNode || nodeMap.size === 0) {
      return label;
    }

    let updatedLabel = label;
    nodeMap.forEach((nodeName, nodeId) => {
      const nodeIdRegex = new RegExp(nodeId, 'g');
      updatedLabel = updatedLabel.replace(nodeIdRegex, nodeName);
    });

    return updatedLabel;
  };

  const transformResponseToChartData = useCallback((response) => {
    if (!response.result || !response.result.labels || !response.result.data) {
      throw new Error('Invalid response format');
    }

    const { labels, data } = response.result;
    const { view, summary: summaryData } = response;

    const deduplicatedSummary = deduplicateSummary(summaryData);
    setSummary(deduplicatedSummary);

    if (onSummaryUpdate && deduplicatedSummary) {
      onSummaryUpdate(context, deduplicatedSummary);
    }

    const nodeMap = buildNodeMapping(summaryData);
    const isGroupedByNode = activeGroupBy.includes('node');

    const metricLabels = labels.slice(1);

    const seenLabels = new Map();
    const datasets = [];

    metricLabels.forEach((label, labelIndex) => {
      const displayLabel = replaceNodeIdsInLabel(label, nodeMap, isGroupedByNode);

      if (seenLabels.has(displayLabel)) {
        const existingIndex = seenLabels.get(displayLabel);
        const existingDataset = datasets[existingIndex];

        data.forEach((row, rowIndex) => {
          const valueArray = row[labelIndex + 1];
          const value = Array.isArray(valueArray) ? valueArray[0] : valueArray;

          const existingPoint = existingDataset.data[rowIndex];
          if (existingPoint) {
            const currentValue = parseFloat(existingPoint.v) || 0;
            const newValue = value || 0;
            existingPoint.v = (currentValue + newValue) / 2;
          }
        });
      } else {
        const seriesData = data.map(row => {
          const timestamp = row[0];
          const valueArray = row[labelIndex + 1];
          const value = Array.isArray(valueArray) ? valueArray[0] : valueArray;

          return {
            dt: new Date(timestamp).toISOString(),
            v: value || 0
          };
        });

        const datasetIndex = datasets.length;
        datasets.push({
          label: displayLabel,
          data: seriesData,
          color: DEFAULT_COLORS[datasetIndex % DEFAULT_COLORS.length]
        });

        seenLabels.set(displayLabel, datasetIndex);
      }
    });

    let unit = view?.units || '';
    if (unit.toLowerCase() === 'percentage') {
      unit = '%';
    }

    return {
      datasets,
      title: view?.title || context || 'Chart',
      context: context || "context not set",
      unit: unit
    };
  }, [context, DEFAULT_COLORS, activeGroupBy, onSummaryUpdate, deduplicateSummary]);

  // Fetch data from API
  const fetchData = useCallback(async (groupByParam, filters) => {
    setLoading(true);
    setError(null);

    try {
      if (!space?.id || !room?.id) {
        throw new Error('Space ID or Room ID not found. Please select a space and room.');
      }

      const currentSummary = summary;
      const { applicableGroupBy, applicableFilters } = getApplicableFilters(
        groupByParam,
        filters,
        currentSummary
      );

      const params = buildGetDataParams(applicableGroupBy, applicableFilters);
      // Token is handled by Rust backend
      const response = await getData(space.id, room.id, params);
      const transformedData = transformResponseToChartData(response);

      setChartData(transformedData);
      setLoading(false);
    } catch (err) {
      console.error('Failed to fetch chart data:', err);
      setError(err.message || 'Failed to load chart data');
      setLoading(false);
    }

  }, [space?.id, room?.id, buildGetDataParams, transformResponseToChartData, getApplicableFilters]);

  // Fetch data when initialized and when filters/grouping changes
  useEffect(() => {
    if (!isInitialized) return;
    fetchData(activeGroupBy, activeFilters);
  }, [isInitialized, activeGroupBy, activeFilters, fetchData]);

  // Parse available options from summary
  const availableOptions = useMemo(() => {
    if (!summary) return { groupByOptions: [], filterOptions: {} };

    const groupByOptions = [];
    const filterOptions = {};

    if (summary.nodes && summary.nodes.length > 1) {
      groupByOptions.push({ label: 'node', displayName: 'Node' });
      filterOptions.node = summary.nodes
        .map(n => ({
          value: n.mg,
          displayName: n.nm || n.mg
        }))
        .sort((a, b) => a.displayName.localeCompare(b.displayName));
    }

    if (summary.dimensions && summary.dimensions.length > 1) {
      groupByOptions.push({ label: 'dimension', displayName: 'Dimension' });
      filterOptions.dimension = summary.dimensions
        .map(d => ({
          value: d.id,
          displayName: d.id
        }))
        .sort((a, b) => a.displayName.localeCompare(b.displayName));
    }

    if (summary.instances && summary.instances.length > 1) {
      groupByOptions.push({ label: 'instance', displayName: 'Instance' });
      filterOptions.instance = summary.instances
        .map(i => ({
          value: i.id,
          displayName: i.id
        }))
        .sort((a, b) => a.displayName.localeCompare(b.displayName));
    }

    if (summary.labels && Array.isArray(summary.labels)) {
      summary.labels.forEach(labelObj => {
        if (labelObj.vl && labelObj.vl.length > 1) {
          groupByOptions.push({ label: labelObj.id, displayName: labelObj.id });
          filterOptions[labelObj.id] = labelObj.vl
            .map(v => ({
              value: v.id,
              displayName: v.id
            }))
            .sort((a, b) => a.displayName.localeCompare(b.displayName));
        }
      });
    }

    return { groupByOptions, filterOptions };
  }, [summary]);

  // Handle group by change
  const handleGroupByChange = useCallback((label, isChecked) => {
    setActiveGroupBy(prev => {
      if (isChecked) {
        return [...prev, label];
      } else {
        return prev.filter(l => l !== label);
      }
    });
  }, []);

  // Handle filter change
  const handleFilterChange = useCallback((filterLabel, value, isChecked) => {
    setActiveFilters(prev => {
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

  // Remove a specific group by
  const handleRemoveGroupBy = useCallback((label) => {
    setActiveGroupBy(prev => prev.filter(l => l !== label));
  }, []);

  // Remove a specific filter
  const handleRemoveFilter = useCallback((filterLabel, value) => {
    setActiveFilters(prev => {
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

  // Reset to original props configuration
  const handleReset = useCallback(() => {
    setActiveGroupBy(normalizedGroupBy);
    setActiveFilters(normalizedFilterBy);
  }, [normalizedGroupBy, normalizedFilterBy]);

  // Check if current state differs from initial props
  const hasChanges = useMemo(() => {
    const normalizedActiveFilters = normalizeFilterMap(activeFilters);
    const normalizedInitialFilters = normalizeFilterMap(normalizedFilterBy);

    if (activeGroupBy.length !== normalizedGroupBy.length) return true;
    if (!activeGroupBy.every(g => normalizedGroupBy.includes(g))) return true;

    const activeFilterKeys = Object.keys(normalizedActiveFilters);
    const normalizedFilterKeys = Object.keys(normalizedInitialFilters);

    if (activeFilterKeys.length !== normalizedFilterKeys.length) return true;

    for (const key of activeFilterKeys) {
      if (!normalizedInitialFilters[key]) return true;
      if (normalizedActiveFilters[key].length !== normalizedInitialFilters[key].length) return true;
      if (!normalizedActiveFilters[key].every(v => normalizedInitialFilters[key].includes(v))) return true;
    }

    return false;
  }, [activeGroupBy, activeFilters, normalizedGroupBy, normalizedFilterBy]);

  // Get display name for a filter value
  const getFilterDisplayName = useCallback((filterLabel, value) => {
    const options = availableOptions.filterOptions[filterLabel];
    if (!options) return value;

    const option = options.find(opt => opt.value === value);
    return option ? option.displayName : value;
  }, [availableOptions]);

  // Calculate skipped filters for display
  const skippedFiltersInfo = useMemo(() => {
    if (!summary) return { groupBy: [], filters: {} };

    const availableLabels = new Set(['node', 'dimension', 'instance']);
    const skippedGroupBy = [];
    const skippedFilters = {};

    if (summary.labels && Array.isArray(summary.labels)) {
      summary.labels.forEach(labelObj => {
        if (labelObj.id) {
          availableLabels.add(labelObj.id);
        }
      });
    }

    activeGroupBy.forEach(label => {
      if (!availableLabels.has(label)) {
        skippedGroupBy.push(label);
      }
    });

    Object.entries(activeFilters).forEach(([filterLabel, values]) => {
      if (!availableLabels.has(filterLabel)) {
        skippedFilters[filterLabel] = values;
        return;
      }

      if (filterLabel === 'node' && summary.nodes) {
        const availableNodeIds = new Set(summary.nodes.map(n => n.mg));
        const skippedValues = values.filter(v => !availableNodeIds.has(v));
        if (skippedValues.length > 0) {
          skippedFilters[filterLabel] = skippedValues;
        }
      } else if (filterLabel === 'dimension' && summary.dimensions) {
        const availableDimIds = new Set(summary.dimensions.map(d => d.id));
        const skippedValues = values.filter(v => !availableDimIds.has(v));
        if (skippedValues.length > 0) {
          skippedFilters[filterLabel] = skippedValues;
        }
      } else if (filterLabel === 'instance' && summary.instances) {
        const availableInstIds = new Set(summary.instances.map(i => i.id));
        const skippedValues = values.filter(v => !availableInstIds.has(v));
        if (skippedValues.length > 0) {
          skippedFilters[filterLabel] = skippedValues;
        }
      } else {
        const labelObj = summary.labels?.find(l => l.id === filterLabel);
        if (labelObj && labelObj.vl) {
          const availableValues = new Set(labelObj.vl.map(v => v.id));
          const skippedValues = values.filter(v => !availableValues.has(v));
          if (skippedValues.length > 0) {
            skippedFilters[filterLabel] = skippedValues;
          }
        } else {
          skippedFilters[filterLabel] = values;
        }
      }
    });

    return { groupBy: skippedGroupBy, filters: skippedFilters };
  }, [summary, activeGroupBy, activeFilters]);

  // Show tooltip with data values and smart positioning
  const showTooltip = useCallback((event, nearestPoints) => {
    if (!nearestPoints || nearestPoints.length === 0) return;

    const validPoints = nearestPoints.filter(np => np && np.point);
    if (validPoints.length === 0) return;

    const timestamp = d3.timeFormat('%Y-%m-%d %H:%M:%S %z')(validPoints[0].point.date);

    const sortedPoints = validPoints.sort((a, b) => b.point.value - a.point.value);
    const limitedPoints = sortedPoints.slice(0, 10);
    const hasMore = sortedPoints.length > 10;

    const dataRows = limitedPoints.map(({ dataset, point }) => ({
      color: dataset.color,
      label: dataset.label,
      value: point.value.toFixed(2),
    }));

    const offset = 15;
    const padding = 10;

    let left = event.clientX + offset;
    let top = event.clientY - offset;

    const windowWidth = window.innerWidth;
    const windowHeight = window.innerHeight;

    const tooltipWidth = 280;
    const baseHeight = 80 + (dataRows.length * 30);
    const tooltipHeight = hasMore ? baseHeight + 25 : baseHeight;

    if (left + tooltipWidth + padding > windowWidth) {
      left = event.clientX - tooltipWidth - offset;
    }

    if (left < padding) {
      left = padding;
    }

    if (top + tooltipHeight + padding > windowHeight) {
      top = event.clientY - tooltipHeight - offset;
    }

    if (top < padding) {
      top = padding;
    }

    setTooltip({
      visible: true,
      x: left,
      y: top,
      content: { timestamp, dataRows, hasMore, moreCount: validPoints.length - 10 },
    });
  }, []);

  // Hide tooltip
  const hideTooltip = useCallback(() => {
    setTooltip({
      visible: false,
      x: 0,
      y: 0,
      content: null,
    });
  }, []);

  // Handle legend item click for series filtering
  const handleLegendClick = useCallback((seriesLabel, event) => {
    const isCtrlOrCmd = event.ctrlKey || event.metaKey;

    setSelectedSeries((prevSelected) => {
      if (!prevSelected) {
        return new Set([seriesLabel]);
      }

      if (isCtrlOrCmd) {
        const newSelected = new Set(prevSelected);
        if (newSelected.has(seriesLabel)) {
          newSelected.delete(seriesLabel);
          return newSelected.size === 0 ? null : newSelected;
        } else {
          newSelected.add(seriesLabel);
          return newSelected;
        }
      }

      if (prevSelected.size === 1 && prevSelected.has(seriesLabel)) {
        return null;
      }

      return new Set([seriesLabel]);
    });
  }, []);

  // Check if a series is selected
  const isSeriesSelected = useCallback((seriesLabel) => {
    if (!selectedSeries) return true;
    return selectedSeries.has(seriesLabel);
  }, [selectedSeries]);

  // ULTRA-OPTIMIZED CANVAS + MINIMAL SVG RENDERING
  useEffect(() => {
    if (!canvasRef.current || !svgRef.current || dimensions.width === 0 || dimensions.height === 0 || !chartData) return;

    try {
      if (!chartData?.datasets || chartData.datasets.length === 0) {
        throw new Error('No data available');
      }

      // Parse and prepare datasets with downsampling
      const datasets = chartData.datasets.map((dataset, index) => {
        if (!dataset.data || dataset.data.length === 0) {
          throw new Error(`Dataset "${dataset.label}" has no data`);
        }

        const parsedData = dataset.data.map((point) => {
          try {
            return {
              date: new Date(point.dt),
              value: parseFloat(point.v),
            };
          } catch (err) {
            throw new Error(`Invalid data point in dataset "${dataset.label}"`);
          }
        });

        parsedData.sort((a, b) => a.date - b.date);

        // Downsample data for performance
        const downsampledData = downsampleData(parsedData, 500);

        return {
          label: dataset.label || `Series ${index + 1}`,
          color: dataset.color || DEFAULT_COLORS[index % DEFAULT_COLORS.length],
          data: downsampledData,
          originalData: parsedData, // Keep original for tooltip
        };
      });

      setError(null);

      // Filter datasets based on selection
      const filteredDatasets = selectedSeries
        ? datasets.filter(d => selectedSeries.has(d.label))
        : datasets;

      const margin = { top: 30, right: 80, bottom: 70, left: 70 };
      const width = dimensions.width - margin.left - margin.right;
      const height = dimensions.height - margin.top - margin.bottom;

      // Setup scales
      const allDates = datasets.flatMap((d) => d.data.map((p) => p.date));
      const xDomain = d3.extent(allDates);

      const allValues = datasets.flatMap((d) => d.data.map((p) => p.value));
      const minValue = Math.min(...allValues);
      const maxValue = Math.max(...allValues);
      const valuePadding = (maxValue - minValue) * 0.1 || Math.abs(maxValue) * 0.1 || 1;

      // Calculate y-domain to include all values (including negative)
      // If all values are non-negative, start from 0 for better visualization
      const yMin = minValue >= 0 ? 0 : minValue - valuePadding;
      const yMax = maxValue + valuePadding;
      const yDomain = [yMin, yMax];

      const xScale = d3.scaleTime().domain(xDomain).range([0, width]);
      const yScale = d3.scaleLinear().domain(yDomain).range([height, 0]);

      // === CANVAS RENDERING ===
      const canvas = canvasRef.current;
      const ctx = canvas.getContext('2d');

      // Calculate full canvas dimensions
      const canvasWidth = width + margin.left + margin.right;
      const canvasHeight = height + margin.top + margin.bottom;

      // Set canvas size with device pixel ratio and zoom for crisp rendering
      // When zoomed in (zoom > 1), render at higher resolution to avoid pixelation
      const baseDP = window.devicePixelRatio || 1;
      const dpr = baseDP * Math.max(zoom, 1);
      canvas.width = canvasWidth * dpr;
      canvas.height = canvasHeight * dpr;
      canvas.style.width = `${canvasWidth}px`;
      canvas.style.height = `${canvasHeight}px`;

      // Reset transform and fill entire canvas with white first
      ctx.setTransform(1, 0, 0, 1, 0, 0);
      ctx.fillStyle = '#ffffff';
      ctx.fillRect(0, 0, canvas.width, canvas.height);

      // Now scale and translate for chart drawing
      ctx.setTransform(dpr, 0, 0, dpr, margin.left * dpr, margin.top * dpr);

      // Performance measurement
      const drawStart = performance.now();

      // Draw all lines on canvas
      filteredDatasets.forEach(dataset => {
        ctx.beginPath();
        ctx.strokeStyle = dataset.color;
        ctx.lineWidth = 2;
        ctx.lineJoin = 'round';
        ctx.lineCap = 'round';

        dataset.data.forEach((point, i) => {
          const x = xScale(point.date);
          const y = yScale(point.value);

          if (i === 0) {
            ctx.moveTo(x, y);
          } else {
            ctx.lineTo(x, y);
          }
        });

        ctx.stroke();
      });

      // === MINIMAL SVG (Axes + Simple Crosshair Only) ===
      const svg = d3.select(svgRef.current);
      svg.selectAll('*').remove();

      svg
        .attr('width', dimensions.width)
        .attr('height', dimensions.height);

      const g = svg
        .append('g')
        .attr('transform', `translate(${margin.left},${margin.top})`);

      // Grid
      g.append('g')
        .attr('class', styles.grid)
        .attr('opacity', 0.1)
        .call(
          d3
            .axisLeft(yScale)
            .ticks(5)
            .tickSize(-width)
            .tickFormat('')
        );

      // Axes
      const xAxis = d3.axisBottom(xScale).ticks(6).tickSizeOuter(0);
      const yAxis = d3
        .axisLeft(yScale)
        .ticks(5)
        .tickFormat((d) => `${d}`)
        .tickSizeOuter(0);

      g.append('g')
        .attr('class', styles.xAxis)
        .attr('transform', `translate(0,${height})`)
        .call(xAxis);

      g.append('text')
        .attr('class', styles.axisLabel)
        .attr('text-anchor', 'middle')
        .attr('x', width / 2)
        .attr('y', height + 50)
        .text('Time');

      g.append('g')
        .attr('class', styles.yAxis)
        .call(yAxis);

      // Y-axis label with unit
      if (chartData.unit) {
        g.append('text')
          .attr('class', styles.yAxisLabel)
          .attr('x', 0)
          .attr('y', -15)
          .text(chartData.unit);
      }

      // === ULTRA-SIMPLIFIED INTERACTION ===
      // Just a crosshair line - NO circles per series
      const crosshair = g
        .append('line')
        .attr('class', styles.crosshair)
        .attr('y1', 0)
        .attr('y2', height)
        .attr('stroke', 'rgba(0, 0, 0, 0.3)')
        .attr('stroke-width', 1)
        .attr('stroke-dasharray', '4,4')
        .style('pointer-events', 'none')
        .style('display', 'none');

      // Interaction overlay
      const overlay = g
        .append('rect')
        .attr('class', styles.overlay)
        .attr('width', width)
        .attr('height', height)
        .style('fill', 'none')
        .style('pointer-events', 'all')
        .style('cursor', 'crosshair');

      // Use requestAnimationFrame for smooth mousemove
      let rafId = null;

      overlay
        .on('mousemove', function (event) {
          if (rafId) {
            cancelAnimationFrame(rafId);
          }

          rafId = requestAnimationFrame(() => {
            // Get mouse coordinates accounting for CSS transforms (React Flow zoom)
            // Use getBoundingClientRect which gives screen coordinates, then convert to element coordinates
            const svgNode = svgRef.current;
            const rect = svgNode.getBoundingClientRect();

            // Calculate scale factor from CSS transforms (rect dimensions vs SVG dimensions)
            const scaleX = rect.width / dimensions.width;

            // Convert screen position to element position
            const screenX = event.clientX - rect.left;
            const mouseX = (screenX / scaleX) - margin.left;

            // Clamp mouseX to the chart bounds
            const clampedX = Math.max(0, Math.min(width, mouseX));
            const date = xScale.invert(clampedX);

            // Show crosshair (no transitions, instant)
            crosshair
              .attr('x1', clampedX)
              .attr('x2', clampedX)
              .style('display', null);

            // Use original data for tooltip precision
            const nearestPoints = filteredDatasets.map((dataset) => {
              const bisect = d3.bisector((d) => d.date).left;
              const dataToUse = dataset.originalData || dataset.data;
              const index = bisect(dataToUse, date, 1);
              const d0 = dataToUse[index - 1];
              const d1 = dataToUse[index];
              if (!d0) return { dataset, point: d1 };
              if (!d1) return { dataset, point: d0 };
              const point = date - d0.date > d1.date - date ? d1 : d0;
              return { dataset, point };
            });

            showTooltip(event, nearestPoints);
          });
        })
        .on('mouseleave', function () {
          if (rafId) {
            cancelAnimationFrame(rafId);
            rafId = null;
          }

          crosshair.style('display', 'none');
          hideTooltip();
        });

    } catch (err) {
      console.error('Chart rendering error:', err);
      setError(err.message);
    }
  }, [dimensions, chartData, selectedSeries, DEFAULT_COLORS, showTooltip, hideTooltip, zoom]);

  // Generate chart title
  const getChartTitle = () => {
    if (chartData?.title && chartData?.context) {
      return `${chartData.title} (${chartData.context})`;
    }
    return context || 'Context Chart';
  };

  if (loading && !summary) {
    return (
      <div ref={containerRef} className={styles.chartContainer}>
        <div className={styles.chartHeader}>
          <h3 className={styles.chartTitle}>{getChartTitle()}</h3>
          {onRemove && (
            <button className={styles.removeButton} onClick={onRemove} title="Remove chart">
              ×
            </button>
          )}
        </div>
        <div className={styles.loadingContainer}>
          <div className={styles.loadingContent}>
            <NetdataSpinner size={40} />
            <div className={styles.loadingText}>Loading chart data...</div>
          </div>
        </div>
      </div>
    );
  }

  if (error) {
    return (
      <div ref={containerRef} className={styles.chartContainer}>
        <div className={styles.chartHeader}>
          <h3 className={styles.chartTitle}>{getChartTitle()}</h3>
          {onRemove && (
            <button className={styles.removeButton} onClick={onRemove} title="Remove chart">
              ×
            </button>
          )}
        </div>
        <div className={styles.errorMessage}>
          <span className={styles.errorIcon}>⚠️</span>
          <span>Chart Error: {error}</span>
        </div>
      </div>
    );
  }

  const { groupByOptions, filterOptions } = availableOptions;
  const hasFilterOptions = groupByOptions.length > 0 || Object.keys(filterOptions).length > 0;

  return (
    <div ref={containerRef} className={styles.chartContainer}>
      <div className={styles.chartHeader}>
        <div className={styles.chartTitleRow}>
          <h3 className={styles.chartTitle}>{getChartTitle()}</h3>
          {onRemove && (
            loading && summary ? (
              <div className={styles.removeButtonSpinner}>
                <div className={styles.loadingSpinner}></div>
              </div>
            ) : (
              <button className={styles.removeButton} onClick={onRemove} title="Remove chart">
                ×
              </button>
            )
          )}
        </div>

        {/* Filters & Grouping Panel */}
        {hasFilterOptions && (
          <div className={styles.filterPanel}>
            <div className={styles.filterPanelHeader}>
              <button
                className={styles.filterPanelToggle}
                onClick={() => setIsFilterPanelOpen(!isFilterPanelOpen)}
              >
                <span className={styles.filterPanelToggleIcon}>
                  {isFilterPanelOpen ? '▼' : '▶'}
                </span>
                <span className={styles.filterPanelTitle}>Chart Filters & Grouping</span>
              </button>
              {hasChanges && (
                <button
                  className={styles.resetButton}
                  onClick={handleReset}
                  title="Reset to original configuration"
                >
                  Reset
                </button>
              )}
            </div>

            {!isFilterPanelOpen && (activeGroupBy.length > 0 || Object.keys(activeFilters).length > 0) && (
              <div className={styles.activeSelections}>
                {activeGroupBy.map(group => {
                  const isSkipped = skippedFiltersInfo.groupBy.includes(group);
                  return (
                    <div key={group} className={`${styles.activeTag} ${isSkipped ? styles.activeTagSkipped : ''}`}>
                      <span className={styles.activeTagPrefix}>Group:</span>
                      <span className={styles.activeTagValue}>
                        {isSkipped && <span className={styles.skippedIcon} title="Not available in this chart">⊘ </span>}
                        {group}
                      </span>
                      <button
                        className={styles.activeTagRemove}
                        onClick={(e) => {
                          e.stopPropagation();
                          handleRemoveGroupBy(group);
                        }}
                        title={`Remove ${group} grouping`}
                      >
                        ×
                      </button>
                    </div>
                  );
                })}

                {Object.entries(activeFilters).map(([filterLabel, values]) =>
                  values.map(value => {
                    const isSkipped = skippedFiltersInfo.filters[filterLabel]?.includes(value);
                    return (
                      <div key={`${filterLabel}-${value}`} className={`${styles.activeTag} ${isSkipped ? styles.activeTagSkipped : ''}`}>
                        <span className={styles.activeTagPrefix}>{filterLabel}:</span>
                        <span className={styles.activeTagValue}>
                          {isSkipped && <span className={styles.skippedIcon} title="Not available in this chart">⊘ </span>}
                          {getFilterDisplayName(filterLabel, value)}
                        </span>
                        <button
                          className={styles.activeTagRemove}
                          onClick={(e) => {
                            e.stopPropagation();
                            handleRemoveFilter(filterLabel, value);
                          }}
                          title={`Remove ${filterLabel} filter`}
                        >
                          ×
                        </button>
                      </div>
                    );
                  })
                )}
              </div>
            )}

            {isFilterPanelOpen && (
              <div className={styles.filterPanelContent}>
                <ChartFilterPanelContent
                  groupByOptions={groupByOptions}
                  filterOptions={filterOptions}
                  activeGroupBy={activeGroupBy}
                  activeFilters={activeFilters}
                  onGroupByChange={handleGroupByChange}
                  onFilterChange={handleFilterChange}
                />
              </div>
            )}
          </div>
        )}

        {/* Legend */}
        {chartData?.datasets && chartData.datasets.length > 0 && (
          <div className={styles.legendWrapper}>
            <div className={styles.legend}>
              {[...chartData.datasets]
                .sort((a, b) => {
                  const labelA = a.label || '';
                  const labelB = b.label || '';
                  return labelA.localeCompare(labelB);
                })
                .map((dataset, index) => {
                  const seriesLabel = dataset.label || `Series ${index + 1}`;
                  const isSelected = isSeriesSelected(seriesLabel);

                  return (
                    <div
                      key={index}
                      className={`${styles.legendItem} ${!isSelected ? styles.legendItemInactive : ''}`}
                      onClick={(e) => handleLegendClick(seriesLabel, e)}
                      style={{ cursor: 'pointer' }}
                      title={`Click to select only ${seriesLabel}, Ctrl+Click to toggle`}
                    >
                      <span
                        className={styles.legendColor}
                        style={{
                          backgroundColor:
                            dataset.color || DEFAULT_COLORS[index % DEFAULT_COLORS.length],
                          opacity: isSelected ? 1 : 0.3,
                        }}
                      ></span>
                      <span className={styles.legendLabel}>{seriesLabel}</span>
                    </div>
                  );
                })}
            </div>
          </div>
        )}
      </div>

      <div className={styles.chartWrapper}>
        {/* Canvas for drawing lines */}
        <canvas
          ref={canvasRef}
          className={styles.chartCanvas}
        />

        {/* SVG overlay for axes and interactions */}
        <svg ref={svgRef} className={styles.chartSvg}></svg>
      </div>

      {/* Tooltip */}
      {tooltip.visible && tooltip.content && ReactDOM.createPortal(
        <div
          ref={tooltipRef}
          className={styles.tooltip}
          style={{
            left: `${tooltip.x}px`,
            top: `${tooltip.y}px`,
          }}
        >
          <div className={styles.tooltipDate}>{tooltip.content.timestamp}</div>
          {tooltip.content.dataRows.map((row, index) => (
            <div key={index} className={styles.tooltipRow}>
              <span
                className={styles.tooltipColor}
                style={{ backgroundColor: row.color }}
              ></span>
              <span className={styles.tooltipLabel}>{row.label}:</span>
              <span className={styles.tooltipValue}>{row.value}</span>
            </div>
          ))}
          {tooltip.content.hasMore && (
            <div className={styles.tooltipMore}>
              +{tooltip.content.moreCount} more series
            </div>
          )}
        </div>,
        document.body
      )}
    </div>
  );
};

export default ContextChart;
