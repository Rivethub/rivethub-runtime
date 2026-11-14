#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        jsmodel,
        reason = "CONTRACT note 185: rh writes this plug-in into $DSH_HOME/plugins as file text; the string is the program dsh loads, not a model of values rh evaluates"
    )
)]
pub(super) const TEMPLATE: &str = r"import { spawn } from 'node:child_process'

const COMMAND = '__COMMAND__'

export const name = 'rivethub-capture'
export const inject = ['sessions']

export function apply(ctx) {
  try {
    ctx.on('session/event', (session, event) => {
      try {
        if (event?.type !== 'turn/end') return
        const id = session?.id || session?.header?.id
        if (typeof id !== 'string' || id.length === 0) return
        const cwd = session?.header?.cwd || session?.cwd
        const payload = JSON.stringify({
          session_id: id,
          hook_event_name: 'turn/end',
          cwd: typeof cwd === 'string' ? cwd : undefined,
        })
        const child = spawn('bash', ['-c', COMMAND, 'rh-capture', payload], {
          stdio: 'ignore',
          detached: true,
        })
        child.on('error', () => {})
        child.unref()
      } catch {}
    })
  } catch {}
}
";
