# TODO

1. Add a Safe/God toggle to the TOML configuration page.
   Safe should load a safer preset.
   God should load the current power-user profile, escalated further for bigger runs and easier expansion into lower-level system access.

2. Keep tightening the local UI.
   Remove the old Models page holdover and fold any model visibility into the pages that still matter for Ollama-first local use.
   Continue simplifying the dashboard/navigation so the interface matches a free local runtime instead of a hosted cost-tracking product.

3. Push Ollama tool-call compatibility harder.
   Accept more malformed tool-call formats from current Ollama models.
   Keep teaching the agent to keep trying tools until a real tool executes instead of stopping at fake JSON or one-shot junk.
