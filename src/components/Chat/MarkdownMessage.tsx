import React, { memo, useMemo } from 'react';
import ReactMarkdown, { type Components, type Options } from 'react-markdown';
import remarkGfm from 'remark-gfm';
import CodeBlock from './CodeBlock';
import MermaidDiagram from './MermaidDiagram';
import VegaChart, { isVegaLiteSpecPath } from './VegaChart';
import WorkspaceImage from './WorkspaceImage';
import { resolveWorkspaceHref, useWorkspaceFileLocation } from './WorkspaceFileContext';
import { isWorkspaceRelativeHref } from '../../utils/htmlBundle';
import styles from './MarkdownMessage.module.css';

interface MarkdownMessageProps {
  content: string;
  // Used by mermaid and vega-lite blocks to defer/debounce rendering while
  // content is still arriving; all other elements render the same
  // regardless of streaming state.
  isStreaming?: boolean;
  // Renderers that replace the defaults for some elements. Pass a stable
  // object: a new one re-renders the whole block.
  components?: Components;
  // Transforms of the rendered tree, as `components`: a stable list.
  rehypePlugins?: Options['rehypePlugins'];
}

// Memoized remark plugins array to prevent recreation
const remarkPlugins = [remarkGfm];

// `![title](charts/q3.vl.json)` embeds a Vega-Lite spec file as a live chart
// and `![shot](screens/1.png)` a workspace image, both resolved relative to
// the enclosing document (see WorkspaceFileContext); `http(s):`/`data:`
// images render as usual.
const isVegaLiteSpecLink = (src: string | undefined): src is string =>
  !!src && isWorkspaceRelativeHref(src) && isVegaLiteSpecPath(src);

// Minimal structural view of the hast node react-markdown hands to
// component renderers; enough to inspect a paragraph's children.
interface HastNodeLike {
  type: string;
  tagName?: string;
  value?: string;
  properties?: Record<string, unknown>;
  children?: HastNodeLike[];
}

// Markdown puts every image inside a <p>, but a chart renders as a block
// (<div>), which is not valid inside <p>. Paragraphs containing a chart link
// (at any depth — it may sit inside a link or emphasis) are rendered as a
// <div> with the paragraph styling instead.
const containsChartLink = (node: HastNodeLike | undefined): boolean =>
  (node?.children ?? []).some(
    (child) =>
      (child.type === 'element' &&
        child.tagName === 'img' &&
        isVegaLiteSpecLink(typeof child.properties?.src === 'string' ? child.properties.src : undefined)) ||
      containsChartLink(child),
  );

// A link to a workspace file opens it in the app's preview panel. It must
// never reach the WebView, which would resolve it against the app origin.
// Without an opener (no workspace UI around) it renders as plain text.
const WorkspaceLink = ({ href, children }: { href: string; children: React.ReactNode }) => {
  const location = useWorkspaceFileLocation();
  const path = resolveWorkspaceHref(location, href);
  const openFile = location?.openFile;
  if (!openFile || !path) {
    return <span title={href}>{children}</span>;
  }
  return (
    <a
      href={href}
      className={styles.link}
      title={path}
      onClick={(event) => {
        event.preventDefault();
        openFile(path);
      }}
      onAuxClick={(event) => event.preventDefault()}
    >
      {children}
    </a>
  );
};

/**
 * MarkdownMessage Component
 *
 * Renders markdown text with support for:
 * - GitHub Flavored Markdown (tables, strikethrough, task lists, etc.)
 * - Code blocks with syntax highlighting
 * - Mermaid diagrams and Vega-Lite charts (fenced blocks / `.vl.json` links)
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
const useMarkdownComponents = (isStreaming: boolean, overrides?: Components): Components =>
  useMemo<Components>(() => ({
    code: ({ className, children, ...props }) => {
      // react-markdown v10 no longer tells inline and block code apart. Block
      // code (fenced or indented) always ends in a newline; inline code never
      // does, since CommonMark turns its line endings into spaces.
      const isInline = !className && !String(children).endsWith('\n');
      // `[\w-]+`: fence languages like `vega-lite` carry a hyphen.
      const match = /language-([\w-]+)/.exec(className || '');
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

      // Vega-Lite blocks render as charts, with the same streaming/error
      // fallbacks as mermaid.
      if (language === 'vega-lite') {
        return (
          <VegaChart
            source={String(children).replace(/\n$/, '')}
            isStreaming={isStreaming}
          />
        );
      }

      return <CodeBlock code={String(children).replace(/\n$/, '')} language={language} />;
    },
    p: ({ children, node }) =>
      containsChartLink(node) ? (
        <div className={styles.paragraph}>{children}</div>
      ) : (
        <p className={styles.paragraph}>{children}</p>
      ),
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
    img: ({ src, alt, title }) => {
      const source = typeof src === 'string' ? src : undefined;
      if (isVegaLiteSpecLink(source)) {
        return <VegaChart specPath={source} />;
      }
      if (source && isWorkspaceRelativeHref(source)) {
        return <WorkspaceImage path={source} alt={alt} title={title} />;
      }
      return <img className={styles.image} src={source} alt={alt} title={title} />;
    },
    a: ({ href, children, node }) => {
      if (containsChartLink(node)) return <>{children}</>;
      if (href && isWorkspaceRelativeHref(href)) {
        return <WorkspaceLink href={href}>{children}</WorkspaceLink>;
      }
      return (
        <a href={href} className={styles.link} target="_blank" rel="noopener noreferrer">
          {children}
        </a>
      );
    },
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
    ...overrides,
  }), [isStreaming, overrides]); // styles object is stable; isStreaming feeds mermaid/vega-lite blocks

export const MarkdownBlock = memo(({ content, isStreaming = false, components: overrides, rehypePlugins }: MarkdownMessageProps) => {
  const components = useMarkdownComponents(isStreaming, overrides);

  return (
    <ReactMarkdown
      remarkPlugins={remarkPlugins}
      rehypePlugins={rehypePlugins}
      components={components}
    >
      {content}
    </ReactMarkdown>
  );
});

MarkdownBlock.displayName = 'MarkdownBlock';

const MarkdownMessage = memo(({ content, isStreaming = false, components }: MarkdownMessageProps) => {
  return (
    <div className={styles.markdownContainer}>
      <MarkdownBlock content={content} isStreaming={isStreaming} components={components} />
    </div>
  );
});

export default MarkdownMessage;
