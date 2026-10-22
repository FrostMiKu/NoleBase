import { Sparkles } from 'lucide-react'

import { useAgentStore } from '@/stores/agent-store'

/**
 * The right sidebar's agent status area: background jobs and subagent
 * runs, gathered from every turn so a long transcript cannot hide them.
 */
export function AgentStatusPanel() {
  const jobs = useAgentStore((state) => state.jobs)
  const activeJobs = jobs.filter((job) => job.status === 'running')

  return (
    <div className="flex h-full flex-col overflow-y-auto" data-testid="agent-status-panel">
      <p className="flex items-center gap-1.5 px-3 pt-2.5 pb-1 text-[10px] font-semibold tracking-wider text-faint uppercase">
        <Sparkles size={11} />
        Background jobs
        <span className="ml-auto font-mono text-[10px]">
          {jobs.length > 0 && `${activeJobs.length} active · ${jobs.length} total`}
        </span>
      </p>

      {jobs.length === 0 && (
        <p className="px-3 py-1 text-[11px] text-faint">
          No background jobs yet. Jobs and subagent runs appear here while the agent works.
        </p>
      )}

      <ul className="space-y-0.5 px-3 pb-3">
        {[...jobs].reverse().map((job) => (
          <li key={job.id} className="flex items-center gap-2 py-0.5 text-[11px]">
            <span
              className={`inline-block h-1.5 w-1.5 shrink-0 rounded-full ${
                job.status === 'running'
                  ? 'animate-pulse bg-accent'
                  : job.status === 'failed'
                    ? 'bg-danger'
                    : job.status === 'cancelled'
                      ? 'bg-faint'
                      : 'bg-ok'
              }`}
            />
            <span
              className={`truncate ${job.status === 'running' ? 'text-fg' : 'text-muted'}`}
              title={job.label}
            >
              {job.label}
            </span>
            {job.turnId !== null && (
              <span className="ml-auto shrink-0 font-mono text-[10px] text-faint">
                #{job.turnId}
              </span>
            )}
          </li>
        ))}
      </ul>
    </div>
  )
}
