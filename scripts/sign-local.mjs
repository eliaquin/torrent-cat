import { execFileSync } from 'node:child_process'
import { fileURLToPath } from 'node:url'

if (process.platform !== 'darwin') {
  throw new Error('This preview currently targets macOS only.')
}
const bundle = fileURLToPath(new URL('../src-tauri/target/release/bundle/macos/TorrentCat.app', import.meta.url))
// A local ad-hoc signature supports normal launch without claiming a Developer ID
// identity or notarization. Distribution signing belongs in a future release job.
execFileSync('/usr/bin/codesign', ['--force', '--deep', '--sign', '-', bundle], { stdio: 'inherit' })
execFileSync('/usr/bin/codesign', ['--verify', '--deep', '--strict', bundle], { stdio: 'inherit' })
console.log(`Locally signed: ${bundle}`)
