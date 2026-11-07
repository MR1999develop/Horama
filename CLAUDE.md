# Istruzioni di sviluppo di Horama

- Visualizzatore di immagini e file RAW in Rust + egui (eframe con glow;
  i test dell'interfaccia usano egui_kittest con wgpu), per Windows e Linux,
  con un server MCP (`src/mcp_server.rs`, avvio con `--mcp`).
- Applicare le skill di progettazione prima di scrivere o rivedere codice:
  - sempre: `rust-egui-desktop-design`, `software-design-core`,
    `image-processing-design`;
  - interfaccia: `ui-design-core`;
  - file non fidati (RAW, ICC, metadati), server MCP, rilascio:
    `security-design`;
  - rotazioni e ritagli salvati su disco, impostazioni: `durable-data-design`;
  - API di Windows, percorsi e processi: `windows-system-design`.
- Controlli: `scripts\check.bat` su Windows, `scripts/check.sh` su Linux
  (formato, clippy con avvisi come errori, test, `cargo deny`, build di
  rilascio); l'esito completo è in `target/check.log`.
- Un ramo di lavoro alla volta, partendo dall'ultimo `main` controllato. La
  CI parte a ogni push di un ramo: passati i controlli locali, l'agente
  pubblica il ramo con il comando delle regole generali e si unisce a `main`
  solo con i controlli locali e la CI del ramo verdi.
- Push di `main`, tag e release li fa l'utente: `main` con
  `_workflow\push-main.bat`, che vuole in `target\check.log` un controllo
  riuscito proprio su quel commit; quindi dopo l'unione si rilancia
  `scripts\check.bat` su `main`. Il ruleset di `main` vuole commit firmati
  (firma SSH già nella configurazione git del repository).
- Documenti: `docs/adr/` (decisioni difficili da cambiare),
  `docs/threat-model.md`; `TIDY.md` (cose rimandate e stato del progetto)
  resta solo sul PC, escluso da git.
- Licenza `GPL-3.0-only`, copyright e autori **MR**.
