import React, { useEffect, useRef, useState } from 'react';
import { Prism as SyntaxHighlighter } from 'react-syntax-highlighter';
import { useAppTheme } from '../../hooks/useAppTheme';
import { codePalette } from './codePalette';
import styles from './CodeBlock.module.css';

interface CodeBlockProps {
  code: string;
  language?: string;
}

// The surface (background, radius) is the stylesheet's: see .codeBlock.
const highlighterStyle: React.CSSProperties = {
  margin: 0,
  padding: '12px 16px',
  fontSize: '13px',
  lineHeight: '1.55',
};

const codeTagStyle: React.CSSProperties = {
  fontFamily: 'var(--font-mono)',
};

const FEEDBACK_MS = 1200;

type CopyState = 'idle' | 'copied' | 'failed';

const STATUS_TEXT: Record<CopyState, string> = {
  idle: '',
  copied: 'Copied',
  failed: 'Copy failed',
};

/** Fenced code block with a copy-to-clipboard button for its raw source. */
const CodeBlock = ({ code, language }: CodeBlockProps) => {
  const theme = useAppTheme();
  const [copyState, setCopyState] = useState<CopyState>('idle');
  const timerRef = useRef<number | null>(null);

  useEffect(() => () => {
    if (timerRef.current) window.clearTimeout(timerRef.current);
  }, []);

  const handleCopy = async () => {
    let next: CopyState = 'copied';
    try {
      await navigator.clipboard.writeText(code);
    } catch {
      next = 'failed';
    }
    setCopyState(next);
    if (timerRef.current) window.clearTimeout(timerRef.current);
    timerRef.current = window.setTimeout(() => setCopyState('idle'), FEEDBACK_MS);
  };

  return (
    <div className={styles.codeBlock}>
      <SyntaxHighlighter
        language={language || 'text'}
        style={codePalette(theme)}
        customStyle={highlighterStyle}
        codeTagProps={{ style: codeTagStyle }}
        PreTag="div"
      >
        {code}
      </SyntaxHighlighter>
      <button
        type="button"
        className={styles.copyButton}
        data-state={copyState}
        onClick={handleCopy}
        aria-label="Copy code"
        title={copyState === 'idle' ? 'Copy code' : STATUS_TEXT[copyState]}
      >
        {copyState === 'copied' ? (
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <polyline points="20 6 9 17 4 12" />
          </svg>
        ) : (
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
            <rect x="9" y="9" width="11" height="11" rx="2" />
            <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
          </svg>
        )}
      </button>
      <span className="sr-only" role="status">{STATUS_TEXT[copyState]}</span>
    </div>
  );
};

export default CodeBlock;
