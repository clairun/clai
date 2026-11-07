import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { DenyReasonField, useDenyReason } from './DenyReasonField';

/** The field and a Deny button, as each request card draws them. */
const Denial = ({ onDeny }: { onDeny: (decision: { kind: 'deny'; reason?: string }) => void }) => {
  const denyReason = useDenyReason();
  return (
    <>
      <DenyReasonField {...denyReason.field} disabled={false} />
      <button type="button" aria-describedby={denyReason.labelId} onClick={() => onDeny(denyReason.decision())}>
        Deny
      </button>
    </>
  );
};

const reasonField = () => screen.getByRole('textbox', { name: 'Reason for denying (optional)' });

describe('DenyReasonField', () => {
  it('is there before any click, without taking focus', () => {
    render(<Denial onDeny={vi.fn()} />);
    expect(reasonField()).toBeVisible();
    expect(reasonField()).toHaveAttribute('placeholder', 'Tell the agent why');
    expect(reasonField()).not.toHaveFocus();
    expect(document.body).toHaveFocus();
  });

  it('denies in one click with no reason when none was typed', async () => {
    const onDeny = vi.fn();
    render(<Denial onDeny={onDeny} />);
    await userEvent.click(screen.getByRole('button', { name: 'Deny' }));
    expect(onDeny).toHaveBeenCalledTimes(1);
    expect(onDeny).toHaveBeenCalledWith({ kind: 'deny' });
  });

  it('denies in one click with the reason trimmed', async () => {
    const onDeny = vi.fn();
    render(<Denial onDeny={onDeny} />);
    await userEvent.type(reasonField(), '  Push after review  ');
    await userEvent.click(screen.getByRole('button', { name: 'Deny' }));
    expect(onDeny).toHaveBeenCalledTimes(1);
    expect(onDeny).toHaveBeenCalledWith({ kind: 'deny', reason: 'Push after review' });
  });

  it('sends no reason when the field holds only whitespace', async () => {
    const onDeny = vi.fn();
    render(<Denial onDeny={onDeny} />);
    await userEvent.type(reasonField(), '   ');
    await userEvent.click(screen.getByRole('button', { name: 'Deny' }));
    expect(onDeny).toHaveBeenCalledWith({ kind: 'deny' });
  });

  it('sends nothing on Enter or Escape in the field', async () => {
    const onDeny = vi.fn();
    render(<Denial onDeny={onDeny} />);
    await userEvent.type(reasonField(), 'no{Enter}{Escape}');
    expect(onDeny).not.toHaveBeenCalled();
    expect(reasonField()).toHaveValue('no');
  });

  it('tells the Deny button a reason can go with it', () => {
    render(<Denial onDeny={vi.fn()} />);
    expect(screen.getByRole('button', { name: 'Deny' })).toHaveAccessibleDescription('Reason for denying (optional)');
  });

  it('says the reason goes to the agent only on a denial, and how much of the limit it uses', async () => {
    render(<Denial onDeny={vi.fn()} />);
    expect(reasonField()).toHaveAccessibleDescription(/Sent to the agent only if you deny\./);
    await userEvent.type(reasonField(), 'abc');
    expect(screen.getByText('3/500 characters')).toBeInTheDocument();
  });

  it('accepts 500 Unicode characters and drops the next, counting an emoji once', () => {
    render(<Denial onDeny={vi.fn()} />);
    // Two UTF-16 code units each, one character for the backend.
    fireEvent.change(reasonField(), { target: { value: '😀'.repeat(501) } });
    expect(reasonField()).toHaveValue('😀'.repeat(500));
    expect(screen.getByText('500/500 characters')).toBeInTheDocument();
  });
});
