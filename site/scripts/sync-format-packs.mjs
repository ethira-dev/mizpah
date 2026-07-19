#!/usr/bin/env node
/**
 * Sync vendored format-v1 packs from crates/mizpah/formats/packs into the
 * marketing site: searchable index + public JSON for on-demand fetch.
 */
import fs from "node:fs"
import path from "node:path"
import { fileURLToPath } from "node:url"

const __dirname = path.dirname(fileURLToPath(import.meta.url))
const siteRoot = path.resolve(__dirname, "..")
const repoRoot = path.resolve(siteRoot, "..")
const packsDir = path.join(repoRoot, "crates/mizpah/formats/packs")
const outIndex = path.join(siteRoot, "src/data/format-index.json")
const outPacksDir = path.join(siteRoot, "public/formats/packs")

function packIdFromFile(parsed) {
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return null
  for (const [key, value] of Object.entries(parsed)) {
    if (key.startsWith("$")) continue
    if (value && typeof value === "object" && !Array.isArray(value)) {
      return { id: key, pack: value }
    }
  }
  return null
}

function main() {
  if (!fs.existsSync(packsDir)) {
    console.error(`sync-format-packs: packs dir not found: ${packsDir}`)
    process.exit(1)
  }

  fs.mkdirSync(path.dirname(outIndex), { recursive: true })
  fs.mkdirSync(outPacksDir, { recursive: true })

  for (const name of fs.readdirSync(outPacksDir)) {
    if (name.endsWith(".json")) {
      fs.unlinkSync(path.join(outPacksDir, name))
    }
  }

  const index = []
  const files = fs
    .readdirSync(packsDir)
    .filter((name) => name.endsWith(".json"))
    .sort()

  for (const file of files) {
    const srcPath = path.join(packsDir, file)
    let parsed
    try {
      parsed = JSON.parse(fs.readFileSync(srcPath, "utf8"))
    } catch (err) {
      console.warn(`sync-format-packs: skip ${file}: ${err.message}`)
      continue
    }

    const extracted = packIdFromFile(parsed)
    if (!extracted) {
      console.warn(`sync-format-packs: skip ${file}: no pack object`)
      continue
    }

    const { id, pack } = extracted
    if (pack.converter != null) {
      continue
    }

    fs.copyFileSync(srcPath, path.join(outPacksDir, file))
    index.push({
      id,
      title: typeof pack.title === "string" ? pack.title : id,
      description:
        typeof pack.description === "string" ? pack.description : "",
      file,
    })
  }

  index.sort((a, b) => a.id.localeCompare(b.id))
  fs.writeFileSync(outIndex, `${JSON.stringify(index, null, 2)}\n`)
  console.log(`sync-format-packs: ${index.length} packs → ${outIndex}`)
}

main()
