import { useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { PendingAskUser } from '../../assistant/sessionStore';

export const OTHER_INDEX = -1;

/**
 * The answer draft for a pending `ask_user` question and its submission,
 * shared by the full panel and the Fleet's compact row.
 *
 * Submission round-trips via `assistant_submit_user_input`, keyed by
 * pendingId alone, so any session's question can be answered from anywhere.
 * The backend's `ask_user_resolved` clears the request; nothing is cleared
 * optimistically, so a rejected answer (e.g. the run already ended) leaves
 * the question up with the error.
 */
export function useAskAnswer(pending: PendingAskUser | null) {
  const [selectedIndex, setSelectedIndex] = useState<number | null>(null);
  // Multi-select questions: the set of checked option indexes, plus
  // whether the "Other" free-text checkbox is on.
  const [selectedIndexes, setSelectedIndexes] = useState<Set<number>>(new Set());
  const [otherChecked, setOtherChecked] = useState(false);
  const [otherText, setOtherText] = useState('');
  const [submitting, setSubmitting] = useState(false);
  const [error, setError] = useState('');
  const previousPendingIdRef = useRef<string | null>(null);

  // Reset the draft when a new request arrives so a stale answer from a
  // previously-answered (or cancelled) question doesn't carry over.
  useEffect(() => {
    if (pending?.pendingId === previousPendingIdRef.current) return;
    previousPendingIdRef.current = pending?.pendingId || null;
    setSelectedIndex(null);
    setSelectedIndexes(new Set());
    setOtherChecked(false);
    setOtherText('');
    setError('');
    setSubmitting(false);
  }, [pending?.pendingId]);

  const options = useMemo(() => pending?.options || [], [pending?.options]);
  const hasOptions = options.length > 0;
  // multiSelect only applies to option-bearing questions; a free-text
  // question is single-answer by nature.
  const isMultiSelect = hasOptions && !!pending?.multiSelect;

  const canSubmit = useMemo(() => {
    if (!pending || submitting) return false;
    if (!hasOptions) return otherText.trim().length > 0;
    if (isMultiSelect) {
      if (selectedIndexes.size > 0) return true;
      return otherChecked && otherText.trim().length > 0;
    }
    if (selectedIndex === null) return false;
    if (selectedIndex === OTHER_INDEX) return otherText.trim().length > 0;
    return true;
  }, [pending, submitting, hasOptions, isMultiSelect, selectedIndexes, otherChecked, selectedIndex, otherText]);

  const send = async (answer: string, selectedOptionIndex: number | null, selectedOptionIndexes: number[] | null) => {
    if (!pending || submitting) return;
    setError('');
    setSubmitting(true);
    try {
      await invoke('assistant_submit_user_input', {
        request: { pendingId: pending.pendingId, answer, selectedOptionIndex, selectedOptionIndexes },
      });
    } catch (err) {
      setError(typeof err === 'string' ? err : err instanceof Error ? err.message : 'Failed to submit answer.');
      setSubmitting(false);
    }
  };

  /** Send the draft. */
  const submit = async () => {
    if (!canSubmit) return;
    if (isMultiSelect) {
      const indexes = [...selectedIndexes].sort((a, b) => a - b);
      const parts = indexes.map((idx) => options[idx]!.label);
      if (otherChecked && otherText.trim()) parts.push(otherText.trim());
      await send(parts.join(', '), null, indexes);
    } else if (!hasOptions || selectedIndex === OTHER_INDEX || selectedIndex === null) {
      await send(otherText.trim(), null, null);
    } else {
      await send(options[selectedIndex]!.label, selectedIndex, null);
    }
  };

  /** Single-select: send one option as the answer, without touching the draft. */
  const submitOption = (index: number) => send(options[index]!.label, index, null);

  return {
    options,
    hasOptions,
    isMultiSelect,
    selectedIndex,
    setSelectedIndex,
    selectedIndexes,
    setSelectedIndexes,
    otherChecked,
    setOtherChecked,
    otherText,
    setOtherText,
    submitting,
    error,
    canSubmit,
    submit,
    submitOption,
  };
}
