/**
 * Contract a Workspace Settings page exposes to the modal shell, which
 * saves the visible page: `validate` first, then `submit`.
 */

export interface SectionResult {
  ok: boolean;
  error?: string;
  /** Set when the submit created a record; the shell moves to it. */
  createdId?: string;
}

export interface SectionHandle {
  validate: () => SectionResult;
  submit: () => Promise<SectionResult>;
}
