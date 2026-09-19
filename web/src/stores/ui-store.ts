import { create } from 'zustand'

export type WorkspaceConnectionState = 'connecting' | 'connected' | 'disconnected'

/**
 * Pure UI/layout state. Server data (notes, documents, tags, ...) belongs
 * to TanStack Query, not here.
 */
export interface UiState {
  leftSidebarOpen: boolean
  rightSidebarOpen: boolean

  leftSidebarSize: number
  rightSidebarSize: number
  agentPanelSize: number

  commandPaletteOpen: boolean

  workspaceConnection: WorkspaceConnectionState
  workspaceResyncing: boolean

  watcherError: string | null

  /** Incremented to request focus on the agent composer. */
  agentFocusTick: number
}

export interface UiActions {
  toggleLeftSidebar: () => void
  toggleRightSidebar: () => void
  setLeftSidebarSize: (size: number) => void
  setRightSidebarSize: (size: number) => void
  setAgentPanelSize: (size: number) => void
  setCommandPaletteOpen: (open: boolean) => void
  setWorkspaceConnection: (state: WorkspaceConnectionState) => void
  setWorkspaceResyncing: (resyncing: boolean) => void
  setWatcherError: (message: string | null) => void
  requestAgentFocus: () => void
}

export type UiStore = UiState & UiActions

export const useUiStore = create<UiStore>()((set) => ({
  leftSidebarOpen: true,
  rightSidebarOpen: true,

  leftSidebarSize: 20,
  rightSidebarSize: 28,
  agentPanelSize: 58,

  commandPaletteOpen: false,

  workspaceConnection: 'connecting',
  workspaceResyncing: false,

  watcherError: null,

  agentFocusTick: 0,

  toggleLeftSidebar: () => set((state) => ({ leftSidebarOpen: !state.leftSidebarOpen })),
  toggleRightSidebar: () => set((state) => ({ rightSidebarOpen: !state.rightSidebarOpen })),
  setLeftSidebarSize: (size) => set({ leftSidebarSize: size }),
  setRightSidebarSize: (size) => set({ rightSidebarSize: size }),
  setAgentPanelSize: (size) => set({ agentPanelSize: size }),
  setCommandPaletteOpen: (open) => set({ commandPaletteOpen: open }),
  setWorkspaceConnection: (connection) => set({ workspaceConnection: connection }),
  setWorkspaceResyncing: (resyncing) => set({ workspaceResyncing: resyncing }),
  setWatcherError: (watcherError) => set({ watcherError }),
  requestAgentFocus: () => set((state) => ({ agentFocusTick: state.agentFocusTick + 1 })),
}))
