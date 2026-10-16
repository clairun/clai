import { useEffect, useRef } from 'react';

/**
 * Contract a Workspace Settings page exposes to the window, which saves the
 * visible page: `validate` first, then `submit`.
 */

export interface SectionResult {
  ok: boolean;
  error?: string;
  /** Set when the submit created a record; the window moves to it. */
  createdId?: string;
}

export interface SectionHandle {
  validate: () => SectionResult;
  submit: () => Promise<SectionResult>;
  /**
   * Leaves one inner level of the page (a drill-in view), if it is in one.
   * Returns false at the page's top level, where Escape closes the window.
   */
  back?: () => boolean;
}

/** Reports a page's dirtiness to the window without depending on the callback's identity. */
export const useReportDirty = (isDirty: boolean, onDirtyChange?: (isDirty: boolean) => void): void => {
  const report = useRef(onDirtyChange);
  useEffect(() => { report.current = onDirtyChange; });
  useEffect(() => { report.current?.(isDirty); }, [isDirty]);
};
