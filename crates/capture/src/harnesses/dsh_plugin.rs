#[cfg_attr(dylint_lib = "rr_lints", expect(jsmodel))]
pub(super) const TEMPLATE: &str = r"import { spawn } from 'node:child_process'

const COMMAND = '__COMMAND__'

export const name = 'rivethub-capture'
export const inject = ['sessions']

function capture(id, event, cwd, eventId) {
  if (typeof id !== 'string' || id.length === 0) return
  try {
    const payload = JSON.stringify({
      session_id: id,
      hook_event_name: event,
      cwd: typeof cwd === 'string' ? cwd : undefined,
      event_id: eventId,
    })
    const child = spawn('bash', ['-c', COMMAND, 'rh-capture', payload], {
      stdio: 'ignore',
      detached: true,
    })
    child.on('error', () => {})
    child.unref()
  } catch {}
}

export function apply(ctx) {
  try {
    ctx.on('session/event', (session, event) => {
      if (event?.type !== 'turn/end') return
      capture(session?.id || session?.header?.id, event.type, session?.header?.cwd || session?.cwd)
    })
    ctx.on('subagent/end', (info) => {
      if (info?.local !== true) return
      capture(info.id, 'subagent/end', undefined, info.runId)
    })
  } catch {}
}
";
