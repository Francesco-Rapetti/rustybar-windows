# RustyBar per Windows

Barra di stato per Windows 11 in Rust, sorella di
[RustyBar per macOS](https://github.com/Francesco-Rapetti/rustybar): stesse idee (un solo processo,
dati letti in-process e a eventi, widget a pill con popup), ma su un PC da gaming. Qui ci sono le
regole per lavorarci.

## Regole fondamentali (dall'utente)

1. **Attribuzione dei commit: solo ed esclusivamente `Francesco-Rapetti`.** Nei commit non deve
   esserci nessuna traccia di Claude: né come autore, né come committer, né come co-autore. Quindi
   niente `Co-Authored-By`, niente "Generated with Claude Code", niente trailer `Claude-Session`,
   niente email `@anthropic.com`, e lo stesso nelle descrizioni delle PR. Autore e committer sono
   sempre `Francesco-Rapetti`.
   Come è garantito (non toglierne nessuno):
   - `user.name = Francesco-Rapetti` e `user.email` nella config locale della repo (la stessa email
     dei commit della versione macOS, così GitHub li collega all'account).
   - `.claude/settings.json`: `attribution` con `commit` e `pr` vuoti e `sessionUrl: false`, così
     Claude Code non aggiunge niente da sé.
   - `.githooks/commit-msg`, attivo con `core.hooksPath = .githooks`: rifiuta un commit con autore
     o committer diverso da `Francesco-Rapetti`, qualunque `Co-authored-by` e le firme di
     Claude/Anthropic nel messaggio. Mai `--no-verify`, mai `--author` diverso.
   - Prima di ogni push, ricontrolla i commit nuovi:
     `git log origin/main..HEAD --format="%an <%ae> | %cn <%ce>%n%b"`.
2. **Leggerezza assoluta.** È un PC da gaming: la barra non deve far perdere nessun FPS e non deve
   pesare in modo misurabile su CPU, GPU e RAM. Ogni scelta passa da qui: nel dubbio la soluzione
   più leggera vince su quella più bella o più comoda. Una funzione che non può essere leggera non
   si fa, o è spenta di default e documentata nel README col suo costo.

## Principi tecnici

Derivati dalla regola 2. Sono il punto di partenza proposto: le scelte grosse (rendering,
animazioni, widget) si confermano con l'utente prima di scrivere codice.

- **Un processo, Win32 nativo**, col crate `windows` (windows-rs). Niente webview (Tauri, Electron,
  WebView2), niente .NET, niente framework GUI che ridisegnano a ogni frame (egui, iced, wgpu).
- **Nessun device GPU.** Si disegna in CPU e solo quando qualcosa cambia: Direct2D su un render
  target software (WIC o DC) con DirectWrite per il testo, in una DIB a 32 bit; la finestra è
  layered (`WS_EX_LAYERED` + `UpdateLayeredWindow`) e DWM la compone come qualunque altra. Niente
  Direct3D, swap chain o DirectComposition finché non serve davvero (vedi le domande aperte).
- **Niente polling quando Windows ha un evento**, come su macOS: `SetWinEventHook` con
  `WINEVENT_OUTOFCONTEXT` per l'app in primo piano, `RegisterPowerSettingNotification` e
  `WM_POWERBROADCAST` per alimentazione e batteria, `NotifyIpInterfaceChange` o
  `INetworkListManager` per la rete, `IMMNotificationClient` + `IAudioEndpointVolumeCallback` per
  l'audio, `WM_SETTINGCHANGE` e `WM_DWMCOLORIZATIONCOLORCHANGED` per tema e accento,
  `WM_DISPLAYCHANGE` per gli schermi, GSMTC (`GlobalSystemMediaTransportControlsSessionManager`)
  per la musica. Ciò che non ha eventi (CPU, GPU, RAM) va su un timer in-process lento, che
  ridisegna solo se il valore mostrato cambia; le letture fitte solo a popup aperto.
- **A riposo il thread principale dorme** in `GetMessageW` / `MsgWaitForMultipleObjectsEx`:
  zero risvegli quando niente cambia. L'orologio si sveglia una volta al minuto, allineato al
  minuto (timer waitable), non ogni secondo.
- **Vietati**: `timeBeginPeriod` (alza la frequenza del timer per tutto il sistema), cicli attivi,
  WMI (fa lavorare `wmiprvse`), processi lanciati a regime (PowerShell, `cmd`, `wmic`), hook
  globali in-context (DLL iniettate nei processi, giochi compresi: rischio anti-cheat), qualunque
  overlay sopra i giochi.
- **Giochi a schermo intero**: la barra si registra come AppBar (`SHAppBarMessage`), così le
  finestre massimizzate non la coprono e arriva `ABN_FULLSCREENAPP`. Con un'app a schermo intero
  sullo schermo della barra: barra nascosta, timer fermi, nessun lavoro fino all'uscita
  (`SHQueryUserNotificationState` per i giochi D3D in esclusiva).
- **Il processo si fa piccolo**: priorità `BELOW_NORMAL_PRIORITY_CLASS`, EcoQoS
  (`ProcessPowerThrottling` con `EXECUTION_SPEED`: sulle CPU ibride va sui core efficienti),
  priorità di memoria bassa. Cache di testo e icone ripulite alla chiusura dei popup, come su
  macOS.
- **Binario**: `windows_subsystem = "windows"`; in release `lto`, `codegen-units = 1`,
  `panic = "abort"`, `strip`, CRT statico (`+crt-static`). Poche dipendenze, ognuna giustificata.
- Popup creati all'apertura e distrutti alla chiusura, non nascosti. Una finestra delle
  impostazioni pesante, se ci sarà, va in un processo suo che finisce quando si chiude (come su
  macOS).

## Obiettivi misurabili

Da misurare dopo ogni cambiamento che può pesare (script `scripts/measure.ps1`, da scrivere):

- RAM: working set privato < 10 MB a riposo (su macOS ~16–20 MB).
- CPU: 0,0% a riposo, nessun risveglio periodico quando niente cambia (orologio a parte).
- GPU: nessun device Direct3D creato dal processo, 0% nel Task Manager.
- 0 processi figli a regime.
- Con un gioco a schermo intero: barra nascosta e 0 risvegli per tutta la sessione.
- Primo frame < 100 ms.

## Regole di lavoro (dalla versione macOS)

- Commit e push solo quando lo chiede l'utente. Commit in stile conventional, in inglese, con un
  corpo che spiega cosa e perché.
- Commenti, README e messaggi di commit in inglese; il commento spiega cosa fa il codice e perché.
  Testi della barra in italiano e inglese.
- **Mai dati veri** in screenshot, mock, test o documentazione: niente nomi, email, dispositivi,
  reti o eventi reali. Dati finti in `examples/mock`.
- **Non cambiare lo stato reale del PC** nelle prove senza chiedere: Wi-Fi, Bluetooth, volume,
  dispositivo audio predefinito, barra delle applicazioni, avvio automatico, impostazioni di
  sistema.

## Ambiente e comandi

- Windows 11 Pro, PowerShell 5.1. Git (`C:\Program Files\Git\cmd`) e Rust (rustup, toolchain MSVC
  con i Build Tools di Visual Studio 2022) installati con winget il 10/11/2026. In una shell aperta
  prima dell'installazione non sono nel PATH: percorso completo o shell nuova.
- Dopo un clone: `git config core.hooksPath .githooks`, `git config user.name "Francesco-Rapetti"`
  e `git config user.email` come sopra.
- Il hook entra nel primo commit eseguibile: `git add --chmod=+x .githooks/commit-msg`.
- `cargo build --release`, `cargo test --release`, `cargo clippy --release` (quando ci sarà il
  codice).

## Domande aperte (da decidere con l'utente)

- Posizione: barra in alto? La barra delle applicazioni di Windows resta, si nasconde da sola o
  viene sostituita?
- Widget della prima versione. Dalla versione macOS: orologio con calendario, app in primo piano,
  workspace (su Windows: desktop virtuali, o un tiling WM come komorebi o GlazeWM?), audio, rete,
  sistema (CPU, GPU, RAM, temperature?), musica, notifiche, eventi del calendario, Home Assistant,
  Docker, Claude; batteria solo se c'è.
- Animazioni: su macOS le fa il window server (Core Animation). Su Windows o costano CPU a ogni
  frame, o vogliono DirectComposition con un device GPU. Proposta: niente animazioni, o solo
  DirectComposition e mai con un gioco aperto.
- Vetro (Mica/Acrylic): DWM lo ricalcola quando cambia ciò che c'è dietro, quindi costa GPU.
  Proposta: tinta piatta di default.
- Scripting Lua (`mlua` su macOS): utile ma pesa; rimandarlo?
- Temi: le stesse 24 palette della versione macOS (Catppuccin e le altre)?

## Stato

- 10/11/2026: repo creata in `E:\Developer\rustybar-windows` (ramo `main`) con questo CLAUDE.md e
  la guardia sulle attribuzioni (config locale, `.claude/settings.json`, hook), provata con
  `git commit` veri in una repo usa e getta.
