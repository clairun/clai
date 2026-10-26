import { createContext, useContext } from 'react';
import type { Components } from 'react-markdown';
import { MarkdownBlock, isVegaLiteSpecLink } from '../Chat/MarkdownMessage';
import styles from './FleetPage.module.css';

/** What a chart chip does: open the workspace, where the chart is drawn. */
export const ChartOpen = createContext<() => void>(() => undefined);

const ChartChip = ({ title }: { title: string }) => {
  const open = useContext(ChartOpen);
  return (
    <button
      type="button"
      className={styles.chartChip}
      title={`Open the workspace to see ${title}`}
      onClick={(e) => {
        e.stopPropagation();
        open();
      }}
      onKeyDown={(e) => e.stopPropagation()}
    >
      <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" aria-hidden="true">
        <path d="M4 20V10M10 20V4M16 20v-7M22 20H2" />
      </svg>
      {title}
    </button>
  );
};

interface Hast {
  value?: string;
  children?: Hast[];
}

const textOf = (node: Hast | undefined): string => node?.value ?? (node?.children ?? []).map(textOf).join('');

/**
 * A miniature draws no charts or diagrams: an embedded chart is a chip, and
 * every code block, mermaid and vega-lite too, is its plain source. Module
 * level, so a bubble's markdown parses again only when its text changes.
 */
export const MINIATURE: Components = {
  img: ({ src, alt, title }) => {
    const source = typeof src === 'string' ? src : undefined;
    return isVegaLiteSpecLink(source) ? <ChartChip title={alt || 'Chart'} /> : <img src={source} alt={alt} title={title} />;
  },
  pre: ({ node }) => (
    <pre className={styles.source}>
      <code>{textOf(node).replace(/\n$/, '')}</code>
    </pre>
  ),
};

/** Main's words in its bubble: markdown, each chart a chip. */
const FleetMarkdown = ({ content }: { content: string }) => <MarkdownBlock content={content} components={MINIATURE} />;

export default FleetMarkdown;
