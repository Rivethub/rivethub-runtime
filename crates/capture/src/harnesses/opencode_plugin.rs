#[cfg_attr(
    dylint_lib = "rr_lints",
    expect(
        jsmodel,
        reason = "CONTRACT notes 125 and 204: rh writes this plug-in file into OpenCode's plugins directory as-is (vendored from rivethub-app integrations/opencode/src/capture.ts at bbd4885); the string is file content rh installs, not a model of values rh evaluates"
    )
)]
pub(super) const TEMPLATE: &str = r"import { spawn } from 'node:child_process'

const COMMAND: string = '__COMMAND__'

const FINISHED = new Set(['session.execution.succeeded', 'session.execution.failed', 'session.execution.interrupted'])

type SessionEvent = {
  type?: string
  location?: { directory?: string }
  data?: { sessionID?: string }
}

type Context = {
  location?: { directory?: string }
  event: { subscribe: (options: { signal: AbortSignal }) => AsyncIterable<SessionEvent> }
}

type ServerInput = { directory?: string }

type ServerEvent = {
  type?: string
  properties?: { sessionID?: string }
}

function capture(sessionID: unknown, directory: string | undefined): void {
  if (typeof sessionID !== 'string' || sessionID.length === 0) return
  try {
    const payload = JSON.stringify({ session_id: sessionID, hook_event_name: 'session.idle', cwd: directory })
    const child = spawn('bash', ['-c', COMMAND, 'rh-capture', payload], { stdio: 'ignore', detached: true })
    child.on('error', () => {})
    child.unref()
  } catch {}
}

async function watch(ctx: Context, signal: AbortSignal): Promise<void> {
  try {
    for await (const event of ctx.event.subscribe({ signal })) {
      if (typeof event?.type !== 'string' || !FINISHED.has(event.type)) continue
      capture(event.data?.sessionID, event.location?.directory ?? ctx.location?.directory)
    }
  } catch {}
}

export default {
  id: 'rivethub-capture',
  setup: (ctx: Context) => {
    const controller = new AbortController()
    void watch(ctx, controller.signal)
    return () => controller.abort()
  },
  server: async (input?: ServerInput) => ({
    event: async (hook?: { event?: ServerEvent }) => {
      const event = hook?.event
      if (event?.type === 'session.idle') capture(event.properties?.sessionID, input?.directory)
    },
  }),
}
";
