import { Bot } from 'lucide-react'

import { useAgentStore } from '@/stores/agent-store'

interface JobRow {
  key: string
  label: string
  status: 'running' | 'finished' | 'failed' | 'preparing'
  turnId: number
}

/** Every background job across the session's turns, newest last. */
function collectJobs(turns: { id: number; tools: { kind: string; key: string; label: string; status: string }[] }[]): JobRow[] {
  const jobs: JobRow[] = []
  for (const turn of turns) {
    for (const tool of turn.tools) {
      if (tool.kind !== 'job') continue
      jobs.push({
        key: `${turn.id}:${tool.key}`,
        label: tool.label,
        status: tool.status as JobRow['status'],
        turnId: turn.id,
      })
    }
  }
  return jobs
}

/**
 * The right sidebar's agent status area: background jobs and subagent
 * runs, gathered from every turn so a long transcript cannot hide them.
 */
export function AgentStatusPanel() {
  const turns = useAgentStore((state) => state.turns)
  const running = useAgentStore((state) => state.running)
  const jobs = collectJobs(turns)
  const activeJobs = jobs.filter((job) => job.status === 'running' || job.status === 'preparing')

  return (
    <div className="flex h-full flex-col overflow-y-auto" data-testid="agent-status-panel">
      <p className="flex items-center gap-1.5 px-3 pt-2.5 pb-1 text-[10px] font-semibold tracking-wider text-faint uppercase">
        <Bot size={11} />
        Agents
        <span className="ml-auto font-mono text-[10px]">
          {running ? 'running' : 'idle'}
          {jobs.length > 0 && ` · ${jobs.length} job(s)`}
        </span>
      </p>

      {activeJobs.length > 0 && (
        <p className="px-3 py-0.5 text-[11px] text-accent">
          {activeJobs.length} active background job(s)
        </p>
      )}

      {jobs.length === 0 && (
        <p className="px-3 py-1 text-[11px] text-faint">
          No background jobs yet. Jobs and subagent runs appear here while the agent works.
        </p>
      )}

      <ul className="space-y-0.5 px-3 pb-3">
        {[...jobs].reverse().map((job) => (
          <li key={job.key} className="flex items-center gap-2 py-0.5 text-[11px]">
            <span
              className={`inline-block h-1.5 w-1.5 shrink-0 rounded-full ${
                job.status === 'running' || job.status === 'preparing'
                  ? 'animate-pulse bg-accent'
                  : job.status === 'failed'
                    ? 'bg-danger'
                    : 'bg-ok'
              }`}
            />
            <span className={`truncate ${job.status === 'running' ? 'text-fg' : 'text-muted'}`} title={job.label}>
              {job.label}
            </span>
            <span className="ml-auto shrink-0 font-mono text-[10px] text-faint">#{job.turnId}</span>
          </li>
        ))}
      </ul>
    </div>
  )
}
