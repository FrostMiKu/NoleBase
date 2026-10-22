import { create } from 'zustand'
import { persist } from 'zustand/middleware'

export type WorkspaceConnectionState = 'connecting' | 'connected' | 'disconnected'

export type SidebarSectionKey = 'notes' | 'archives' | 'daily'

/**
 * Pure UI/layout state. Server data (notes, documents, tags, ...) belongs
 * to TanStack Query, not here.
 */
export interface UiState {
  leftSidebarOpen: boolean
  rightSidebarOpen: boolean

  leftSidebarSize: number
  rightSidebarSize: number
  /** Percentage of the right sidebar's vertical split given to Todos. */
  todoPanelSize: number

  /** Expanded state of the left sidebar's collapsible sections. */
  sidebarSections: Record<SidebarSectionKey, boolean>

  createNoteOpen: boolean
  mobilePanel: 'files' | 'tools' | null
  commandPaletteOpen: boolean

  workspaceConnection: WorkspaceConnectionState
  workspaceResyncing: boolean

  watcherError: string | null

  /** Incremented to request focus on the agent composer. */
  agentFocusTick: number
}

export interface UiActions {
  setCreateNoteOpen: (open: boolean) => void
  setMobilePanel: (panel: 'files' | 'tools' | null) => void
  toggleLeftSidebar: () => void
  toggleRightSidebar: () => void
  setLeftSidebarSize: (size: number) => void
  setRightSidebarSize: (size: number) => void
  setTodoPanelSize: (size: number) => void
  toggleSidebarSection: (key: SidebarSectionKey) => void
  setCommandPaletteOpen: (open: boolean) => void
  setWorkspaceConnection: (state: WorkspaceConnectionState) => void
  setWorkspaceResyncing: (resyncing: boolean) => void
  setWatcherError: (message: string | null) => void
  requestAgentFocus: () => void
}

export type UiStore = UiState & UiActions

export const useUiStore = create<UiStore>()(
  persist(
    (set) => ({
      leftSidebarOpen: true,
      rightSidebarOpen: true,

      leftSidebarSize: 20,
      rightSidebarSize: 28,
      todoPanelSize: 50,

      // The file list opens with Notes visible; Archives and Daily collapsed
      // (the daily river in the center view remains the browsing surface).
      sidebarSections: { notes: true, archives: false, daily: false },

      commandPaletteOpen: false,
      createNoteOpen: false,
      mobilePanel: null,

      workspaceConnection: 'connecting',
      workspaceResyncing: false,

      watcherError: null,

      agentFocusTick: 0,

      setCreateNoteOpen: (open) => set({ createNoteOpen: open }),
      setMobilePanel: (mobilePanel) => set({ mobilePanel }),
      toggleLeftSidebar: () => set((state) => ({ leftSidebarOpen: !state.leftSidebarOpen })),
      toggleRightSidebar: () => set((state) => ({ rightSidebarOpen: !state.rightSidebarOpen })),
      setLeftSidebarSize: (size) => set({ leftSidebarSize: size }),
      setRightSidebarSize: (size) => set({ rightSidebarSize: size }),
      setTodoPanelSize: (size) => set({ todoPanelSize: size }),
      toggleSidebarSection: (key) =>
        set((state) => ({
          sidebarSections: { ...state.sidebarSections, [key]: !state.sidebarSections[key] },
        })),
      setCommandPaletteOpen: (open) => set({ commandPaletteOpen: open }),
      setWorkspaceConnection: (connection) => set({ workspaceConnection: connection }),
      setWorkspaceResyncing: (resyncing) => set({ workspaceResyncing: resyncing }),
      setWatcherError: (watcherError) => set({ watcherError }),
      requestAgentFocus: () => set((state) => ({ agentFocusTick: state.agentFocusTick + 1 })),
    }),
    {
      name: 'nole:layout',
      partialize: (state) => ({
        leftSidebarOpen: state.leftSidebarOpen,
        rightSidebarOpen: state.rightSidebarOpen,
        leftSidebarSize: state.leftSidebarSize,
        rightSidebarSize: state.rightSidebarSize,
        todoPanelSize: state.todoPanelSize,
        sidebarSections: state.sidebarSections,
      }),
    },
  ),
)
