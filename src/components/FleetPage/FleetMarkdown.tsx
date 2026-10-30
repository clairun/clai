import { createContext, useContext, type ReactNode } from 'react';
import type { Components } from 'react-markdown';
import { MarkdownBlock } from '../Chat/MarkdownMessage';
import { isWorkspaceRelativeHref } from '../../utils/htmlBundle';
import styles from './FleetPage.module.css';

/** What a chip does: open the workspace, where its chart or image is drawn. */
export const ChartOpen = createContext<() => void>(() => undefined);

const Chip = ({ title }: { title: string }) => {
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
    >
      <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" aria-hidden="true">
        <path d="M4 20V10M10 20V4M16 20v-7M22 20H2" />
      </svg>
      {title}
    </button>
  );
};

interface Hast {
  type?: string;
  tagName?: string;
  properties?: object;
  value?: string;
  children?: Hast[];
}

const textOf = (node: Hast | undefined): string => node?.value ?? (node?.children ?? []).map(textOf).join('');

/**
 * A miniature draws no charts, diagrams or workspace images: an embedded chart
 * or image is a chip, and every code block, mermaid and vega-lite too, is its
 * plain source. Module level, so a bubble's markdown parses again only when
 * its text changes.
 */
export const MINIATURE: Components = {
  img: ({ src, alt, title }) => {
    const source = typeof src === 'string' ? src : undefined;
    return source && isWorkspaceRelativeHref(source) ? <Chip title={alt || source} /> : <img src={source} alt={alt} title={title} />;
  },
  pre: ({ node }) => (
    <pre className={styles.source}>
      <code>{textOf(node).replace(/\n$/, '')}</code>
    </pre>
  ),
};

/** What a bubble's message begins with: its time, read as part of its first line. */
const Lead = createContext<ReactNode>(null);

/**
 * The lead goes inline at the start of the first paragraph, on its line and
 * baseline and counted in the bubble's width; before any other first block,
 * as a line of its own.
 */
const leadFirstLine = () => (tree: Required<Pick<Hast, 'children'>>) => {
  const lead: Hast = { type: 'element', tagName: 'time', properties: {}, children: [] };
  // Raw HTML, drawn as its text, is a first block too.
  const first = tree.children.find((node) => node.type !== 'text' || node.value?.trim());
  if (first?.tagName === 'p') first.children?.unshift(lead, { type: 'text', value: ' ' });
  else tree.children.unshift({ type: 'element', tagName: 'p', properties: {}, children: [lead] });
};

const LEAD = [leadFirstLine];
const LeadTime = () => useContext(Lead);
const BUBBLE: Components = { ...MINIATURE, time: LeadTime };

/** A bubble's words: markdown, each chart a chip, led by `lead`. */
const FleetMarkdown = ({ content, lead }: { content: string; lead: ReactNode }) => (
  <Lead.Provider value={lead}>
    <MarkdownBlock content={content} components={BUBBLE} rehypePlugins={LEAD} />
  </Lead.Provider>
);

export default FleetMarkdown;
