import React, { memo, useMemo } from 'react';
import ReactMarkdown, { type Components } from 'react-markdown';
import remarkGfm from 'remark-gfm';
import { Prism as SyntaxHighlighter } from 'react-syntax-highlighter';
import { oneLight } from 'react-syntax-highlighter/dist/esm/styles/prism';
import MermaidDiagram from './MermaidDiagram';
import styles from './MarkdownMessage.module.css';

interface MarkdownMessageProps {
  content: string;
  // Used by mermaid blocks to defer/debounce diagram rendering while
  // content is still arriving; all other elements render the same
  // regardless of streaming state.
  isStreaming?: boolean;
}

// Memoized code block styles to prevent recreation
const codeBlockStyle: React.CSSProperties = {
  margin: '12px 0',
  padding: '12px 16px',
  background: 'rgba(0, 0, 0, 0.04)',
  border: '1px solid rgba(0, 0, 0, 0.1)',
  borderRadius: '6px',
  fontSize: '14px',
  lineHeight: '1.5',
};

const codeTagStyle: React.CSSProperties = {
  fontFamily: "'Monaco', 'Menlo', 'Ubuntu Mono', 'Consolas', 'source-code-pro', monospace",
};

// Memoized remark plugins array to prevent recreation
const remarkPlugins = [remarkGfm];

/**
 * MarkdownMessage Component
 *
 * Renders markdown text with support for:
 * - GitHub Flavored Markdown (tables, strikethrough, task lists, etc.)
 * - Code blocks with syntax highlighting
 * - Inline code
 * - Links, bold, italic, lists
 * - Streaming cursor for real-time text
 *
 * Props:
 * - content: The markdown text to render
 * - isStreaming: Whether the message is currently being streamed
 *
 * Performance: Memoized to prevent re-renders when content hasn't changed.
 * The components object is also memoized to prevent ReactMarkdown from
 * re-processing on every render.
 */
const MarkdownMessage = memo(({ content, isStreaming = false }: MarkdownMessageProps) => {
  // Memoize the components object to prevent ReactMarkdown from re-rendering
  // when the parent re-renders but content hasn't changed
  const components = useMemo<Components>(() => ({
    // Customize rendering of specific elements. react-markdown v10 dropped
    // `inline` from the official code-renderer prop type, but still passes
    // it at runtime; read it through a widened local type.
    code: ({ className, children, ...props }) => {
      const { inline } = props as { inline?: boolean };
      // More reliable check: inline code doesn't have className and children is simple text
      const isInline = inline !== false && !className;
      const match = /language-(\w+)/.exec(className || '');
      const language = match ? match[1] : '';

      if (isInline) {
        return (
          <code className={styles.inlineCode} {...props}>
            {children}
          </code>
        );
      }

      // Mermaid blocks render as diagrams (with raw-source fallback
      // while streaming or on parse errors).
      if (language === 'mermaid') {
        return (
          <MermaidDiagram
            code={String(children).replace(/\n$/, '')}
            isStreaming={isStreaming}
          />
        );
      }

      // Code block with syntax highlighting
      return (
        <SyntaxHighlighter
          language={language || 'text'}
          style={oneLight}
          customStyle={codeBlockStyle}
          codeTagProps={{ style: codeTagStyle }}
          PreTag="div"
        >
          {String(children).replace(/\n$/, '')}
        </SyntaxHighlighter>
      );
    },
    p: ({ children }) => <p className={styles.paragraph}>{children}</p>,
    h1: ({ children }) => <h1 className={styles.heading1}>{children}</h1>,
    h2: ({ children }) => <h2 className={styles.heading2}>{children}</h2>,
    h3: ({ children }) => <h3 className={styles.heading3}>{children}</h3>,
    h4: ({ children }) => <h4 className={styles.heading4}>{children}</h4>,
    h5: ({ children }) => <h5 className={styles.heading5}>{children}</h5>,
    h6: ({ children }) => <h6 className={styles.heading6}>{children}</h6>,
    ul: ({ children }) => <ul className={styles.unorderedList}>{children}</ul>,
    ol: ({ children }) => <ol className={styles.orderedList}>{children}</ol>,
    li: ({ children }) => <li className={styles.listItem}>{children}</li>,
    blockquote: ({ children }) => <blockquote className={styles.blockquote}>{children}</blockquote>,
    a: ({ href, children }) => (
      <a href={href} className={styles.link} target="_blank" rel="noopener noreferrer">
        {children}
      </a>
    ),
    table: ({ children }) => (
      <div className={styles.tableWrapper}>
        <table className={styles.table}>{children}</table>
      </div>
    ),
    thead: ({ children }) => <thead className={styles.tableHead}>{children}</thead>,
    tbody: ({ children }) => <tbody className={styles.tableBody}>{children}</tbody>,
    tr: ({ children }) => <tr className={styles.tableRow}>{children}</tr>,
    th: ({ children }) => <th className={styles.tableHeader}>{children}</th>,
    td: ({ children }) => <td className={styles.tableCell}>{children}</td>,
    strong: ({ children }) => <strong className={styles.bold}>{children}</strong>,
    em: ({ children }) => <em className={styles.italic}>{children}</em>,
    del: ({ children }) => <del className={styles.strikethrough}>{children}</del>,
    hr: () => <hr className={styles.horizontalRule} />,
  }), [isStreaming]); // styles object is stable; isStreaming feeds mermaid blocks

  return (
    <div className={styles.markdownContainer}>
      <ReactMarkdown
        remarkPlugins={remarkPlugins}
        components={components}
      >
        {content}
      </ReactMarkdown>
    </div>
  );
});

export default MarkdownMessage;
