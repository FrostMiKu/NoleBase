import { create } from 'zustand'

export type WorkspaceConnectionState = 'connecting' | 'connected' | 'disconnected'

export type SidebarSectionKey = 'daily' | 'notes' | 'archives' | 'tags'

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

  /** Expanded state of the left sidebar's collapsible sections. */
  sidebarSections: Record<SidebarSectionKey, boolean>

  commandPaletteOpen: boolean

  workspaceConnection: WorkspaceConnectionState
  workspaceResyncing: boolean

  watcherError: string | null

  /**
   * The most recently open document path, kept only as a revisit hint for
   * the activity rail's Editor button. The selected document itself stays
   * router-authoritative; opening a document always navigates via the
   * router, so no second copy of the selection can drift.
   */
  lastDocumentPath: string | null

  /** Incremented to request focus on the agent composer. */
  agentFocusTick: number
}

export interface UiActions {
  toggleLeftSidebar: () => void
  toggleRightSidebar: () => void
  setLeftSidebarSize: (size: number) => void
  setRightSidebarSize: (size: number) => void
  setAgentPanelSize: (size: number) => void
  toggleSidebarSection: (key: SidebarSectionKey) => void
  setCommandPaletteOpen: (open: boolean) => void
  setWorkspaceConnection: (state: WorkspaceConnectionState) => void
  setWorkspaceResyncing: (resyncing: boolean) => void
  setWatcherError: (message: string | null) => void
  setLastDocumentPath: (path: string | null) => void
  requestAgentFocus: () => void
}

export type UiStore = UiState & UiActions

export const useUiStore = create<UiStore>()((set) => ({
  leftSidebarOpen: true,
  rightSidebarOpen: true,

  leftSidebarSize: 20,
  rightSidebarSize: 28,
  agentPanelSize: 58,

  sidebarSections: { daily: true, notes: true, archives: true, tags: true },

  commandPaletteOpen: false,

  workspaceConnection: 'connecting',
  workspaceResyncing: false,

  watcherError: null,

  lastDocumentPath: null,

  agentFocusTick: 0,

  toggleLeftSidebar: () => set((state) => ({ leftSidebarOpen: !state.leftSidebarOpen })),
  toggleRightSidebar: () => set((state) => ({ rightSidebarOpen: !state.rightSidebarOpen })),
  setLeftSidebarSize: (size) => set({ leftSidebarSize: size }),
  setRightSidebarSize: (size) => set({ rightSidebarSize: size }),
  setAgentPanelSize: (size) => set({ agentPanelSize: size }),
  toggleSidebarSection: (key) =>
    set((state) => ({
      sidebarSections: { ...state.sidebarSections, [key]: !state.sidebarSections[key] },
    })),
  setCommandPaletteOpen: (open) => set({ commandPaletteOpen: open }),
  setWorkspaceConnection: (connection) => set({ workspaceConnection: connection }),
  setWorkspaceResyncing: (resyncing) => set({ workspaceResyncing: resyncing }),
  setWatcherError: (watcherError) => set({ watcherError }),
  setLastDocumentPath: (lastDocumentPath) => set({ lastDocumentPath }),
  requestAgentFocus: () => set((state) => ({ agentFocusTick: state.agentFocusTick + 1 })),
}))
