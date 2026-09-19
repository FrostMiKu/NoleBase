import { useNavigate, useRouterState } from '@tanstack/react-router'
import { Bot, FileText, Hash, Search } from 'lucide-react'

import { useUiStore } from '@/stores/ui-store'

type RailView = 'editor' | 'tags' | 'agent' | 'search'

interface RailItem {
  view: RailView
  label: string
  icon: typeof FileText
}

const ITEMS: RailItem[] = [
  { view: 'editor', label: 'Editor', icon: FileText },
  { view: 'tags', label: 'Tags', icon: Hash },
  { view: 'agent', label: 'Agent', icon: Bot },
  { view: 'search', label: 'Search', icon: Search },
]

function activeView(pathname: string): RailView | null {
  if (pathname.startsWith('/document')) return 'editor'
  if (pathname === '/tags' || pathname.startsWith('/tag/')) return 'tags'
  if (pathname === '/agent') return 'agent'
  if (pathname === '/search') return 'search'
  return null
}

/**
 * The vertical activity rail (leftmost strip). Switches the center area
 * between the document editor, the tag-centred file lists, the TUI-style
 * main Agent view, and global search — all through router navigation, so
 * the dirty guard and URL state keep working unchanged.
 */
export function ActivityRail() {
  const navigate = useNavigate()
  const pathname = useRouterState({ select: (state) => state.location.pathname })
  const active = activeView(pathname)
  const lastDocumentPath = useUiStore((state) => state.lastDocumentPath)

  const go = (view: RailView) => {
    switch (view) {
      case 'editor':
        // Reopen the most recent document; with none, fall back home.
        if (lastDocumentPath !== null) {
          void navigate({ to: '/document', search: { path: lastDocumentPath } })
        } else {
          void navigate({ to: '/' })
        }
        return
      case 'tags':
        void navigate({ to: '/tags' })
        return
      case 'agent':
        void navigate({ to: '/agent' })
        return
      case 'search':
        void navigate({ to: '/search' })
        return
    }
  }

  return (
    <nav
      className="flex w-11 shrink-0 flex-col items-center gap-1 border-r border-border bg-panel py-2"
      aria-label="Activity rail"
      data-testid="activity-rail"
    >
      {ITEMS.map(({ view, label, icon: Icon }) => {
        const isActive = active === view
        return (
          <button
            key={view}
            type="button"
            title={label}
            aria-label={label}
            aria-current={isActive ? 'page' : undefined}
            onClick={() => go(view)}
            className={`relative flex h-9 w-9 items-center justify-center rounded-sm transition-colors ${
              isActive ? 'bg-accent/20 text-accent-strong' : 'text-muted hover:bg-raised hover:text-fg'
            }`}
          >
            {isActive && (
              <span className="absolute left-[-6px] h-5 w-0.5 rounded-full bg-accent" aria-hidden />
            )}
            <Icon size={17} />
          </button>
        )
      })}
    </nav>
  )
}
