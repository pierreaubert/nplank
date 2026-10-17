# Real image support

Images are delivered as bytes, not just paths.

## Flow

1. **Attach** — drop files on the conversation (Finder), use Attach…,
   or paste: `Cmd+V` with an image on the clipboard attaches it when the
   composer sees the key first; `Cmd+Shift+V` (PasteImage action) is the
   reliable path because the text Input consumes `Cmd+V`. Pasted images
   are written to `<data dir>/pasted-images/`.
2. **Chips** — image attachments render with a 24px thumbnail
   (`gpui::img` on the file) plus name/size and the usual remove button.
3. **Submit** — attachments are sniffed by magic bytes
   (`detect_image_media_type`, ≤10 MiB). Real images become
   `Command::Prompt.images: Vec<PromptImage>` (base64; the field is
   `#[serde(default)]` so old clients still parse). Non-images keep the
   existing path manifest.

## Per-backend delivery (daemon side)

- **Local Plank** — bytes are materialized content-addressed into
  `<workspace>/.nplank-images/` and referenced in the prompt as
  `[Image #n] path (media)`; the engine's `view_image` tool opens them,
  mirroring plank's own image-paste design (its engine is text-only).
- **Claude Code** — true content delivery: `--input-format stream-json`
  with base64 image blocks when images are present (plain stdin text
  otherwise).
- **Codex** — materialized files passed via `codex exec --image <file>`.
- **Muse** — materialized file references in text (no documented
  image input item in its RPC).
- **Remote Plank** — bytes cannot cross the remote protocol yet; the
  prompt carries an explicit note naming the untransferred images
  instead of silently dropping them.
- **Demo** — the echo notes the attachment count.

Tests: legacy-prompt serde compatibility (`model`), materialize +
reference + remote note (`workers::tests::prompt_images_materialize_and_reference`).
