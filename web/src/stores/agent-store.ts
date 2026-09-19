import { create } from 'zustand'

import type { AgentConnectionState } from '@/types/agent'
import type { AgentEvent } from '@/types/events'
import { beginAgentRun, initialAgentState, reduceAgentEvent, type AgentState } from '@/features/agent/agent-reducer'

/**
 * Realtime agent projection. WebSocket events flow through the pure
 * `reduceAgentEvent`; server snapshots (`GET /api/agent`) sync `running`
 * on (re)connect. Streaming text never enters TanStack Query.
 */
export interface AgentStore extends AgentState {
  connection: AgentConnectionState

  dispatchEvent: (event: AgentEvent) => void
  beginRun: () => void
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

  dispatchEvent: (event) => set((state) => reduceAgentEvent(state, event)),
  beginRun: () => set((state) => beginAgentRun(state)),
  setConnection: (connection) => set({ connection }),
  setRunning: (running) => set({ running }),
  markDisconnected: () => set({ connection: 'disconnected', streamInterrupted: true }),
  clearApproval: () => set({ approval: null }),
  clearAskUser: () => set({ askUser: null }),
  clearTerminalInput: () => set({ terminalInput: null }),
  dismissInterruption: () => set({ streamInterrupted: false }),
}))
