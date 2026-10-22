import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, expect, it, vi } from 'vitest'
import { SearchPage } from './SearchPage'
const navigate = vi.hoisted(() => vi.fn())
vi.mock('@tanstack/react-router', () => ({ useNavigate: () => navigate }))
vi.mock('@/queries/workspace', () => ({ useSearchResults: () => ({ isSuccess: false }) }))
afterEach(() => {
  cleanup()
  vi.useRealTimers()
  vi.clearAllMocks()
})
it('cancels stale typed queries when browser navigation changes the URL query', () => {
  vi.useFakeTimers()
  const view = render(<SearchPage q="first" />)
  fireEvent.change(screen.getByRole('textbox'), { target: { value: 'typing' } })
  view.rerender(<SearchPage q="back navigation" />)
  act(() => vi.advanceTimersByTime(500))
  expect(screen.getByRole('textbox')).toHaveValue('back navigation')
  expect(navigate).not.toHaveBeenCalled()
})
it('cancels pending search navigation when leaving the page', () => {
  vi.useFakeTimers()
  const view = render(<SearchPage q="" />)
  fireEvent.change(screen.getByRole('textbox'), { target: { value: 'typing' } })
  view.unmount()
  act(() => vi.advanceTimersByTime(500))
  expect(navigate).not.toHaveBeenCalled()
})
