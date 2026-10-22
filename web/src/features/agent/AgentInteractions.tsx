import { useState } from 'react'
import { ShieldAlert, TerminalSquare, UserRound } from 'lucide-react'

import { buttonStyles } from '@/components/ui/dialog'
import { useAgentStore } from '@/stores/agent-store'
import {
  useAgentAnswer,
  useAgentApproval,
  useAgentCancelAnswer,
  useAgentCancelTerminalInput,
  useAgentTerminalInput,
} from '@/queries/agent'
import type { ApprovalKind } from '@/types/agent'

/**
 * High-priority pending interaction area: approval, ask-user, and private
 * terminal input. Each is strictly user-initiated; nothing auto-approves.
 */
export function AgentInteractions() {
  const approval = useAgentStore((state) => state.approval)
  const askUser = useAgentStore((state) => state.askUser)
  const terminalInput = useAgentStore((state) => state.terminalInput)

  // Nothing pending: render nothing at all, so no stray separator or
  // padding appears above the composer.
  if (approval === null && askUser === null && terminalInput === null) return null

  return (
    <div className="flex max-h-[60%] shrink-0 flex-col gap-2 overflow-y-auto border-t border-border bg-panel px-3 py-2">
      {approval !== null && (
        <ApprovalBox kind={approval.kind} title={approval.title} message={approval.message} />
      )}
      {askUser !== null && (
        <AskUserBox key={askUser.question} question={askUser.question} options={askUser.options} />
      )}
      {terminalInput !== null && (
        <TerminalInputBox
          key={terminalInput.prompt}
          purpose={terminalInput.purpose}
          prompt={terminalInput.prompt}
        />
      )}
    </div>
  )
}

function KindBadge({ kind }: { kind: ApprovalKind }) {
  const label =
    kind.kind === 'command'
      ? 'Command'
      : kind.kind === 'diff'
        ? 'Diff'
        : kind.kind === 'destructive_confirm'
          ? 'Destructive'
          : 'Confirm'
  const danger = kind.kind === 'destructive_confirm' || kind.kind === 'command'
  return (
    <span
      className={`rounded-sm border px-1 py-px font-mono text-[9px] uppercase ${
        danger ? 'border-danger/50 text-danger' : 'border-border-strong text-muted'
      }`}
    >
      {label}
    </span>
  )
}

function ApprovalBox({
  kind,
  title,
  message,
}: {
  kind: ApprovalKind
  title: string
  message: string
}) {
  const approval = useAgentApproval()
  const clearApproval = useAgentStore((state) => state.clearApproval)

  const respond = (decision: 'approve' | 'deny') => {
    approval.mutate(decision, { onSuccess: clearApproval })
  }

  return (
    <section
      className="rounded-sm border border-warn/50 bg-warn/10 p-2"
      data-testid="agent-approval"
    >
      <p className="flex items-center gap-1.5 text-[11px] font-semibold text-warn">
        <ShieldAlert size={12} />
        {title}
        <KindBadge kind={kind} />
      </p>
      <p className="mt-1 text-[11px] whitespace-pre-wrap text-fg">{message}</p>
      {kind.kind === 'command' && (
        <div className="mt-1.5 rounded-sm border border-border bg-bg px-2 py-1.5">
          <p className="font-mono text-[10px] text-faint">{kind.purpose}</p>
          <p className="mt-0.5 font-mono text-[11px] text-fg">{kind.label}</p>
          {kind.code !== '' && (
            <pre className="mt-1 overflow-x-auto font-mono text-[10px] text-muted">{kind.code}</pre>
          )}
        </div>
      )}
      {approval.isError && (
        <p role="alert" className="mt-2 text-xs text-danger">
          {approval.error.message}
        </p>
      )}
      <div className="mt-2 flex justify-end gap-1.5">
        <button
          type="button"
          className={buttonStyles.danger}
          disabled={approval.isPending}
          onClick={() => respond('deny')}
        >
          Deny
        </button>
        <button
          type="button"
          className={buttonStyles.primary}
          disabled={approval.isPending}
          onClick={() => respond('approve')}
        >
          {approval.isPending ? '…' : 'Approve'}
        </button>
      </div>
    </section>
  )
}

function AskUserBox({ question, options }: { question: string; options: string[] }) {
  const [value, setValue] = useState('')
  const answer = useAgentAnswer()
  const cancelAnswer = useAgentCancelAnswer()
  const clearAskUser = useAgentStore((state) => state.clearAskUser)
  const busy = answer.isPending || cancelAnswer.isPending

  return (
    <section
      className="rounded-sm border border-accent/50 bg-accent/10 p-2"
      data-testid="agent-ask-user"
    >
      <p className="flex items-center gap-1.5 text-[11px] font-semibold text-accent-strong">
        <UserRound size={12} />
        Agent question
      </p>
      <p className="mt-1 text-[11px] whitespace-pre-wrap text-fg">{question}</p>
      {options.length > 0 && (
        <div className="mt-1.5 flex flex-wrap gap-1">
          {options.map((option) => (
            <button
              key={option}
              type="button"
              className="rounded-sm border border-border-strong bg-raised px-1.5 py-0.5 text-[10px] text-fg transition-colors hover:bg-overlay"
              disabled={busy}
              onClick={() => answer.mutate(option, { onSuccess: clearAskUser })}
            >
              {option}
            </button>
          ))}
        </div>
      )}
      <div className="mt-2 flex items-center gap-1.5">
        <input
          aria-label="Your answer"
          type="text"
          value={value}
          autoFocus
          onChange={(event) => setValue(event.target.value)}
          onKeyDown={(event) => {
            if (event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229) return
            if (event.key === 'Enter' && value.trim() !== '' && !busy) {
              answer.mutate(value, { onSuccess: clearAskUser })
            }
          }}
          placeholder="Your answer…"
          className="h-6 w-full rounded-sm border border-border-strong bg-bg px-2 text-[11px] text-fg outline-none focus:border-accent"
        />
      </div>
      {(answer.isError || cancelAnswer.isError) && (
        <p role="alert" className="mt-2 text-xs text-danger">
          {(answer.error ?? cancelAnswer.error)?.message}
        </p>
      )}
      <div className="mt-1.5 flex justify-end gap-1.5">
        <button
          type="button"
          className={buttonStyles.secondary}
          disabled={busy}
          onClick={() => cancelAnswer.mutate(undefined, { onSuccess: clearAskUser })}
        >
          Cancel
        </button>
        <button
          type="button"
          className={buttonStyles.primary}
          disabled={busy || value.trim() === ''}
          onClick={() => answer.mutate(value, { onSuccess: clearAskUser })}
        >
          Submit
        </button>
      </div>
    </section>
  )
}

function TerminalInputBox({ purpose, prompt }: { purpose: string; prompt: string }) {
  const [value, setValue] = useState('')
  const terminalInput = useAgentTerminalInput()
  const cancelTerminalInput = useAgentCancelTerminalInput()
  const clearTerminalInput = useAgentStore((state) => state.clearTerminalInput)
  const busy = terminalInput.isPending || cancelTerminalInput.isPending

  return (
    <section
      className="rounded-sm border border-border-strong bg-raised p-2"
      data-testid="agent-terminal-input"
    >
      <p className="flex items-center gap-1.5 text-[11px] font-semibold text-muted">
        <TerminalSquare size={12} />
        Agent terminal input requested
        {purpose !== '' && <span className="font-mono text-[10px] text-faint">({purpose})</span>}
      </p>
      <p className="mt-1 text-[11px] text-fg">{prompt}</p>
      <input
        type="password"
        aria-label="Private terminal input"
        autoComplete="off"
        spellCheck={false}
        value={value}
        autoFocus
        onChange={(event) => setValue(event.target.value)}
        onKeyDown={(event) => {
          if (event.nativeEvent.isComposing || event.nativeEvent.keyCode === 229) return
          if (event.key === 'Enter' && !busy) {
            terminalInput.mutate(value, { onSuccess: clearTerminalInput })
          }
        }}
        placeholder="Terminal input…"
        className="mt-1.5 h-6 w-full rounded-sm border border-border-strong bg-bg px-2 font-mono text-[11px] text-fg outline-none focus:border-accent"
      />
      {(terminalInput.isError || cancelTerminalInput.isError) && (
        <p role="alert" className="mt-2 text-xs text-danger">
          {(terminalInput.error ?? cancelTerminalInput.error)?.message}
        </p>
      )}
      <div className="mt-1.5 flex justify-end gap-1.5">
        <button
          type="button"
          className={buttonStyles.secondary}
          disabled={busy}
          onClick={() => cancelTerminalInput.mutate(undefined, { onSuccess: clearTerminalInput })}
        >
          Cancel
        </button>
        <button
          type="button"
          className={buttonStyles.primary}
          disabled={busy}
          onClick={() => terminalInput.mutate(value, { onSuccess: clearTerminalInput })}
        >
          Send
        </button>
      </div>
    </section>
  )
}
