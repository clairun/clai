import React, { useState } from 'react';
import MarkdownMessage from './MarkdownMessage';
import styles from './ToolBlock.module.css';

/**
 * ToolBlock Component
 *
 * Displays tool_use and tool_result content blocks in a collapsible format.
 * Similar to claude.ai's tool visualization.
 *
 * The tool is considered "running" when we have toolUse but no toolResult.
 * Once toolResult is received, the tool is marked as "complete".
 */

interface ToolUse {
  id?: string;
  name: string;
  input?: Record<string, unknown> | null;
}

interface ToolResult {
  id?: string;
  text?: string | null;
}

interface ToolBlockProps {
  toolUse: ToolUse;
  toolResult?: ToolResult | null;
}

const ToolBlock = ({ toolUse, toolResult }: ToolBlockProps) => {
  const [isExpanded, setIsExpanded] = useState(false);

  // Determine if the tool is currently running
  // A tool is running when we have toolUse but no toolResult
  const isRunning = !toolResult;

  const toggleExpanded = () => {
    setIsExpanded(!isExpanded);
  };

  // Format JSON input for display
  const formatJsonInput = (input: unknown): string => {
    try {
      return JSON.stringify(input, null, 2);
    } catch {
      return String(input);
    }
  };

  return (
    <div className={styles.toolBlock}>
      <div className={styles.toolHeader} onClick={toggleExpanded}>
        <div className={styles.toolHeaderLeft}>
          <span className={styles.toolIcon}>⚙</span>
          <span className={styles.toolName}>{toolUse.name}</span>
          {isRunning && (
            <span className={styles.runningIndicator}>
              <span className={styles.spinner}></span>
              Running...
            </span>
          )}
          {!isRunning && (
            <span className={styles.completedIndicator}>
              <span className={styles.completedIcon}>✓</span>
              Complete
            </span>
          )}
        </div>
        <div className={styles.toolHeaderRight}>
          <span className={`${styles.expandIcon} ${isExpanded ? styles.expanded : ''}`}>
            ▼
          </span>
        </div>
      </div>

      {isExpanded && (
        <div className={styles.toolContent}>
          {/* Tool Input */}
          {toolUse.input && Object.keys(toolUse.input).length > 0 && (
            <div className={styles.toolSection}>
              <div className={styles.toolSectionTitle}>Input</div>
              <div className={styles.toolSectionContent}>
                <pre className={styles.jsonDisplay}>
                  <code>{formatJsonInput(toolUse.input)}</code>
                </pre>
              </div>
            </div>
          )}

          {/* Tool Result */}
          {toolResult && (
            <div className={styles.toolSection}>
              <div className={styles.toolSectionTitle}>Result</div>
              <div className={styles.toolSectionContent}>
                <div className={styles.toolResult}>
                  <MarkdownMessage
                    content={toolResult.text || ''}
                    isStreaming={false}
                  />
                </div>
              </div>
            </div>
          )}

          {/* No result yet - tool is still running */}
          {!toolResult && (
            <div className={styles.toolSection}>
              <div className={styles.toolSectionContent}>
                <div className={styles.runningMessage}>
                  <span className={styles.runningSpinner}></span>
                  <span>Executing tool...</span>
                </div>
              </div>
            </div>
          )}
        </div>
      )}
    </div>
  );
};

export default ToolBlock;
