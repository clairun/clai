import React, { useEffect, useRef, useState } from 'react';
import { Prism as SyntaxHighlighter } from 'react-syntax-highlighter';
import { oneLight } from 'react-syntax-highlighter/dist/esm/styles/prism';
import styles from './CodeBlock.module.css';

interface CodeBlockProps {
  code: string;
  language?: string;
}

const highlighterStyle: React.CSSProperties = {
  margin: 0,
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

const FEEDBACK_MS = 1200;

type CopyState = 'idle' | 'copied' | 'failed';

const STATUS_TEXT: Record<CopyState, string> = {
  idle: '',
  copied: 'Copied',
  failed: 'Copy failed',
};

/** Fenced code block with a copy-to-clipboard button for its raw source. */
const CodeBlock = ({ code, language }: CodeBlockProps) => {
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
        style={oneLight}
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
