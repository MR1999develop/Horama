# Threat model di Horama

Aggiornato a ogni modifica che cambia un confine di fiducia: formati letti, server MCP,
programmi avviati, file scritti, dipendenze e rilascio.

## Cosa costruiamo

Un visualizzatore di immagini e RAW per desktop (Windows e Ubuntu), eseguito come utente
normale, senza rete. Legge cartelle e file scelti dall'utente, scrive solo:

- i file che l'utente ruota o salva (rotazione senza perdita, "Salva una copia", "Sovrascrivi
  l'originale" solo per i formati non RAW);
- impostazioni, valutazioni e log nella cartella di configurazione dell'utente;
- il Cestino del sistema.

## Asset

| Taglia | Asset |
|---|---|
| L | Le foto dell'utente, RAW compresi (mai modificati), e le loro valutazioni |
| L | L'integrità della build e dei rilasci (chi scarica `horama.exe`) |
| M | Disponibilità dell'app (un file non deve chiuderla né bloccarla) |
| S | Impostazioni, log (contengono percorsi di file, nient'altro) |

## Confini di fiducia

```mermaid
flowchart LR
    F[File immagine e RAW<br/>da chiunque] -->|decoder| H(Horama)
    M[Client MCP<br/>deciso da un modello] -->|JSON-RPC su stdin| H
    A[Riga di comando,<br/>trascinamento] -->|percorsi| H
    H -->|avvio per percorso assoluto| P[explorer.exe, rundll32.exe,<br/>app di "Apri con"]
    H -->|scrittura atomica| D[(File dell'utente,<br/>impostazioni)]
    S[Dipendenze, CI] -->|build| H
```

## Minacce e risposte

| Elemento | Minaccia | Risposta |
|---|---|---|
| File immagine | Decoder che va in panic su un file malformato | Decoder in Rust, `panic = "unwind"` e `catch_unwind` attorno a ogni decodifica: il file dà un errore, l'app resta aperta; ogni panic è scritto nel log |
| File immagine | Memoria esaurita (file enorme, immagine con dimensioni dichiarate enormi) | File letti per intero solo fino a 1 GiB, controllato sul file aperto; limite di memoria dei decoder (512 MiB per immagine); RAW sviluppati solo sotto 200 MP e se la memoria libera basta |
| File RAW | Contenuto che non è un RAW (file danneggiato) | Riconosciuto dalla firma e segnalato come danneggiato; per le miniature non vi si cerca nemmeno un'anteprima |
| Client MCP | Chiamate malformate o ostili decise da un modello | Ogni chiamata diventa un comando tipizzato prima di arrivare all'interfaccia: campi sconosciuti, valori fuori intervallo e testi oltre i limiti sono rifiutati; righe oltre 1 MiB scartate; gli errori non ripetono più di 200 caratteri dell'input |
| Client MCP | Modifica dei file | Solo `rotate_image` scrive, senza perdita e con gli stessi comandi dell'interfaccia; i RAW non sono mai modificati |
| Client MCP | Dati dei file letti come istruzioni dal modello | I metadati tornano come valori JSON, dichiarati come dati nella descrizione del tool |
| Programmi esterni | Un programma con lo stesso nome eseguito al posto di quello di sistema | Avvio per percorso assoluto (cartella di Windows, `/usr/bin`), argomenti separati, nessuna shell |
| Scritture | File troncato da un crash | File temporaneo nella stessa cartella, `sync_all`, poi rinomina |
| Dipendenze e CI | Crate vulnerabile, licenza incompatibile, sorgente non attesa, action alterata | `cargo deny` in ogni controllo e ogni giorno in CI; `Cargo.lock` e `--locked`; action fissate a uno SHA; token della CI in sola lettura; Dependabot con attesa sulle versioni nuove |

## Rischi accettati

- **Decoder nello stesso processo**, non isolati: sono in Rust (memory-safe salvo le dipendenze
  con `unsafe`). Da rivedere se si aggiunge un decoder scritto in C.
- **Il server MCP vede tutto ciò che vede l'utente**: può aprire ogni cartella leggibile. Il
  client MCP è il programma che avvia Horama, quindi agisce già come l'utente. Da rivedere se
  il server ascoltasse su un socket o in rete: servirebbero autenticazione e controllo
  dell'`Origin`.
- **"Apri con" su Windows** chiama COM direttamente (`unsafe`, giustificato blocco per blocco).
  Da rivedere se si adotta il crate `windows`.
- **Rilasci senza firma Authenticode** né `SHA256SUMS` firmati: da aggiungere prima di
  distribuire l'app fuori da GitHub.

## Fuori ambito

Un attaccante che controlla già l'account dell'utente o la macchina.
