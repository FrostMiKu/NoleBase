import { act, cleanup, fireEvent, render, screen } from '@testing-library/react'
import { afterEach, expect, it } from 'vitest'
import { clearSubmittedDraft, useDraft } from './use-draft'
function Composer() {
  const [value, setValue] = useDraft('test')
  return (
    <input aria-label="Draft" value={value} onChange={(event) => setValue(event.target.value)} />
  )
}
afterEach(() => {
  cleanup()
  sessionStorage.clear()
})
it('shares a restored draft and clears it when a submission finishes after navigation', () => {
  render(<Composer />)
  fireEvent.change(screen.getByLabelText('Draft'), { target: { value: 'Submitted' } })
  cleanup()
  render(<Composer />)
  expect(screen.getByLabelText('Draft')).toHaveValue('Submitted')
  act(() => clearSubmittedDraft('test', 'Submitted'))
  expect(screen.getByLabelText('Draft')).toHaveValue('')
})
it('never clears newer text when an older submission finishes', () => {
  render(<Composer />)
  fireEvent.change(screen.getByLabelText('Draft'), { target: { value: 'New thought' } })
  act(() => clearSubmittedDraft('test', 'Old thought'))
  expect(screen.getByLabelText('Draft')).toHaveValue('New thought')
})
