import { describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { DenyReasonPanel, useDenial } from './DenyReasonPanel';

/** A Deny button and its panel, as each request card draws them. */
const Denial = ({ onConfirm, submitting = false }: { onConfirm: (reason?: string) => void; submitting?: boolean }) => {
  const denial = useDenial();
  return (
    <>
      <button type="button" {...denial.denyButton}>
        Deny
      </button>
      {denial.open && (
        <DenyReasonPanel
          id={denial.panelId}
          confirmLabel="Deny command"
          submitting={submitting}
          onConfirm={onConfirm}
          onClose={denial.close}
        />
      )}
    </>
  );
};

const reasonField = () => screen.getByRole('textbox', { name: /reason for denying/i });

describe('DenyReasonPanel', () => {
  it('opens on Deny and confirms with the reason trimmed', async () => {
    const onConfirm = vi.fn();
    render(<Denial onConfirm={onConfirm} />);
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole('button', { name: 'Deny' }));
    expect(onConfirm).not.toHaveBeenCalled();
    await userEvent.type(reasonField(), '  Push after review  ');
    await userEvent.click(screen.getByRole('button', { name: 'Deny command' }));
    expect(onConfirm).toHaveBeenCalledWith('Push after review');
  });

  it('moves focus into the reason field, and Escape closes it back on Deny', async () => {
    const user = userEvent.setup();
    const onConfirm = vi.fn();
    render(<Denial onConfirm={onConfirm} />);
    const deny = screen.getByRole('button', { name: 'Deny' });
    expect(deny).not.toHaveAttribute('aria-controls');

    deny.focus();
    await user.keyboard('{Enter}');
    const reason = reasonField();
    expect(reason).toHaveFocus();
    expect(deny).toHaveAttribute('aria-expanded', 'true');
    expect(document.getElementById(deny.getAttribute('aria-controls')!)).toContainElement(reason);

    await user.keyboard('{Escape}');
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
    expect(deny).toHaveFocus();
    expect(deny).toHaveAttribute('aria-expanded', 'false');
    expect(onConfirm).not.toHaveBeenCalled();
  });

  it('stays open on Escape while the denial is being sent', async () => {
    render(<Denial onConfirm={vi.fn()} submitting />);
    await userEvent.click(screen.getByRole('button', { name: 'Deny' }));
    fireEvent.keyDown(reasonField(), { key: 'Escape' });
    expect(reasonField()).toBeDisabled();
    expect(screen.getByRole('button', { name: 'Submitting…' })).toBeDisabled();
  });

  it('closes with Cancel or a second Deny, sending nothing', async () => {
    const user = userEvent.setup();
    const onConfirm = vi.fn();
    render(<Denial onConfirm={onConfirm} />);
    const deny = screen.getByRole('button', { name: 'Deny' });

    await user.click(deny);
    await user.click(screen.getByRole('button', { name: 'Cancel' }));
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
    expect(deny).toHaveFocus();

    await user.click(deny);
    await user.click(deny);
    expect(screen.queryByRole('textbox')).not.toBeInTheDocument();
    expect(onConfirm).not.toHaveBeenCalled();
  });

  it('sends no reason when the field holds only whitespace', async () => {
    const onConfirm = vi.fn();
    render(<Denial onConfirm={onConfirm} />);
    await userEvent.click(screen.getByRole('button', { name: 'Deny' }));
    await userEvent.type(reasonField(), '   ');
    await userEvent.click(screen.getByRole('button', { name: 'Deny command' }));
    expect(onConfirm).toHaveBeenCalledWith(undefined);
  });

  it('says the reason goes to the agent, and how much of the limit it uses', async () => {
    render(<Denial onConfirm={vi.fn()} />);
    await userEvent.click(screen.getByRole('button', { name: 'Deny' }));
    expect(reasonField()).toHaveAccessibleDescription(/Sent to the agent with this response\./);
    await userEvent.type(reasonField(), 'abc');
    expect(screen.getByText('3/500 characters')).toBeInTheDocument();
  });

  it('accepts 500 Unicode characters and drops the next, counting an emoji once', async () => {
    render(<Denial onConfirm={vi.fn()} />);
    await userEvent.click(screen.getByRole('button', { name: 'Deny' }));
    // Two UTF-16 code units each, one character for the backend.
    fireEvent.change(reasonField(), { target: { value: '😀'.repeat(501) } });
    expect((reasonField() as HTMLTextAreaElement).value).toBe('😀'.repeat(500));
    expect(screen.getByText('500/500 characters')).toBeInTheDocument();
  });
});
