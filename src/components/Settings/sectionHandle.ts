/**
 * Contract every Workspace Settings section exposes to the modal shell.
 * The modal validates all dirty sections first (atomic gate) and only then
 * submits them, so both steps are separate imperative calls.
 */

export interface SectionResult {
  ok: boolean;
  error?: string;
}

export interface SectionHandle {
  validate: () => SectionResult;
  submit: () => Promise<SectionResult>;
}
