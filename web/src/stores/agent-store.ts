import { create } from 'zustand'

import type { AgentConnectionState, AgentHistoryEntry } from '@/types/agent'
import type { AgentEvent } from '@/types/events'
import {
  beginAgentRun,
  initialAgentState,
  reduceAgentEvent,
  seedAgentHistory,
  type AgentState,
  type JobRun,
} from '@/features/agent/agent-reducer'

/**
 * Realtime agent projection. WebSocket events flow through the pure
 * `reduceAgentEvent`; server snapshots sync `running`/permission/jobs on
 * (re)connect, and the persisted server transcript seeds the turn history
 * (`seedHistory`) the way the TUI restores its panel. Turns accumulate — a
 * new run appends to the transcript instead of overwriting it. Streaming
 * text never enters TanStack Query.
 */
export interface AgentStore extends AgentState {
  runSubmitting: boolean
  connection: AgentConnectionState

  dispatchEvent: (event: AgentEvent) => void
  beginRun: (prompt: string) => void
  /** Replace the transcript with the server's persisted history. */
  seedHistory: (entries: AgentHistoryEntry[], running: boolean, messageCount: number) => void
  /** Replace the session job registry from a server snapshot. */
  setJobs: (jobs: JobRun[]) => void
  setConnection: (connection: AgentConnectionState) => void
  setRunning: (running: boolean) => void
  markDisconnected: () => void
  clearApproval: () => void
  clearAskUser: () => void
  clearTerminalInput: () => void
  dismissInterruption: () => void
}

export const useAgentStore = create<AgentStore>()((set) => ({
  ...initialAgentState(),
  connection: 'connecting',
  runSubmitting: false,

  dispatchEvent: (event) => set((state) => reduceAgentEvent(state, event)),
  beginRun: (prompt) => set((state) => beginAgentRun(state, prompt)),
  seedHistory: (entries, running, messageCount) =>
    set((state) => seedAgentHistory(state, entries, running, messageCount)),
  setJobs: (jobs) => set({ jobs }),
  setConnection: (connection) => set({ connection }),
  setRunning: (running) => set({ running }),
  markDisconnected: () => set({ connection: 'disconnected', streamInterrupted: true }),
  clearApproval: () => set({ approval: null }),
  clearAskUser: () => set({ askUser: null }),
  clearTerminalInput: () => set({ terminalInput: null }),
  dismissInterruption: () => set({ streamInterrupted: false }),
}))
