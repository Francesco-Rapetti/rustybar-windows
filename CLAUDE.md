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

Derivati dalla regola 2 e dalle decisioni prese con l'utente (sotto). Una scelta grossa nuova si
conferma con l'utente prima di scrivere codice.

- **Un processo, Win32 nativo**, col crate `windows` (windows-rs). Niente webview (Tauri, Electron,
  WebView2), niente .NET, niente framework GUI che ridisegnano a ogni frame (egui, iced, wgpu).
- **Si disegna solo quando qualcosa cambia, le animazioni le fa il compositore.** Finestre
  `WS_EX_NOREDIRECTIONBITMAP` con un albero DirectComposition: il contenuto di pill e popup è
  disegnato con Direct2D e DirectWrite in superfici DirectComposition solo quando cambia;
  posizione, opacità, scala e ritaglio si animano con `IDCompositionAnimation`, che esegue DWM da
  solo (come Core Animation su macOS: la barra non lavora durante un'animazione). Il device
  Direct3D 11 è WARP (niente VRAM, rasterizza in CPU solo al cambio di contenuto): misurato
  contro quello hardware, costa un settimo della RAM (vedi Stato). Animazioni spente con un
  gioco a schermo intero e quando Windows ha le animazioni disattivate
  (`SPI_GETCLIENTAREAANIMATION`). Niente swap chain, niente rendering continuo, niente vetro
  (Mica/Acrylic).
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
  (`SHQueryUserNotificationState` per i giochi D3D in esclusiva); device e superfici
  DirectComposition rilasciati, si rifanno all'uscita.
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

Da misurare dopo ogni cambiamento che può pesare, con `scripts\measure.ps1`:

- RAM: working set privato < 10 MB a riposo (su macOS ~16–20 MB).
- CPU: 0,0% a riposo, nessun risveglio periodico quando niente cambia (orologio a parte).
- GPU: 0% a riposo nel Task Manager; lavoro GPU solo durante un'animazione (in DWM); con un gioco
  a schermo intero nessuna risorsa GPU tenuta dalla barra.
- 0 processi figli a regime, salvo `claude` del widget omonimo (vedi sotto).
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
  sistema, desktop virtuali (crearne o toglierne).
- **L'utente può star giocando mentre lavori** (il 10/11/2026 c'era WoW a schermo intero sullo
  schermo principale): niente prove che rubano il primo piano o coprono lo schermo (finestre di
  prova a schermo intero, `SetForegroundWindow`) e niente AppBar di prova che sposta le finestre,
  senza chiedere prima. Le prove normali vanno con una barra di prova a `reserve = false`.

## Ambiente e comandi

- Windows 11 Pro, PowerShell 5.1. Git (`C:\Program Files\Git\cmd`) e Rust (rustup, toolchain MSVC
  con i Build Tools di Visual Studio 2022) installati con winget il 10/11/2026. In una shell aperta
  prima dell'installazione non sono nel PATH: percorso completo o shell nuova.
- Dopo un clone: `git config core.hooksPath .githooks`, `git config user.name "Francesco-Rapetti"`
  e `git config user.email` come sopra.
- Il hook è nella repo come eseguibile (`git add --chmod=+x`, già fatto nel primo commit).
- In PowerShell il messaggio di commit va in un file con fine riga LF e poi `git commit -F <file>`:
  una here-string passata a `-F -` non arriva su stdin.
- `cargo build --release`, `cargo test --release`, `cargo clippy --release` (zero avvisi).
  `cargo` è in `%USERPROFILE%\.cargo\bin` (in Git Bash: `export PATH="$HOME/.cargo/bin:$PATH"`).
- Barra di prova accanto a quella vera: una cartella con un `rustybar.toml` con
  `[bar] reserve = false` (non sposta le finestre), poi `RUSTYBAR_CONFIG_DIR=<cartella>` e
  `target\release\rustybar.exe`; con la stessa variabile `rustybar quit` chiude lei sola (le
  variabili d'ambiente di PowerShell non restano fra un comando e l'altro: rimetterla ogni volta).
  Log in `%LOCALAPPDATA%\rustybar\rustybar-<nome cartella>.log` (quella vera: `rustybar.log`), col
  tempo del primo frame.
- `scripts\measure.ps1 [-Id <pid>] [-Seconds 30]`: memoria privata, CPU, risvegli (cambi di
  contesto dei thread), GPU (motori e memoria), thread, handle, figli e con cosa disegna (WARP o
  driver GPU). Usa le classi WMI delle prestazioni, che hanno lo stesso nome in ogni lingua
  (`Get-Counter` no: qui Windows è in italiano).
- `RUSTYBAR_GPU=hardware`: device Direct3D hardware invece di WARP, per confrontare.
- Screenshot della barra: `System.Drawing` `CopyFromScreen` sulla striscia in alto, poi guardarli.

## Com'è fatta

- `app.rs`: tutto sul main thread. Il ciclo dorme in `MsgWaitForMultipleObjectsEx` su: messaggi,
  i handle dei widget (timer, registro) e la cartella della config. Window procedure e hook
  mettono solo un `Event` in coda; il ciclo la svuota dopo il dispatch, così niente rientra
  nell'app mentre lavora. Una finestra nascosta (`RustyBarControl`, titolo = cartella della
  config) riceve i broadcast (schermi, tema, ora, risveglio, `TaskbarCreated`, `rustybar quit`).
  Barra nascosta su ogni schermo → device rilasciati (`Gfx` a `None`), si rifanno quando una barra
  torna. Animazioni solo se il tema le vuole, Windows le ha accese, nessun gioco della lista in
  primo piano e tutte le barre visibili.
- `bar.rs`: una barra per schermo (`WS_EX_NOREDIRECTIONBITMAP`, topmost, senza attivazione),
  AppBar registrata dopo il primo frame (Explorer ci mette ~200 ms a spostare le finestre),
  `fullscreen_here` (finestra in primo piano che copre lo schermo senza barra del titolo, o
  `QUNS_RUNNING_D3D_FULL_SCREEN`). Per pill un visual con lo sfondo come contenuto, l'evidenziazione
  e il testo in visual figli; firme per ridisegnare solo ciò che cambia; scivolamenti con
  `Gfx::slide` (cubica ease-out eseguita da DWM).
- `gfx.rs` (D3D11 WARP, D2D, DirectComposition, `draw` in DIP alla scala dello schermo), `ui.rs`
  (`Pill` di `Item` di testo, misurati con DirectWrite), `theme.rs` (le 24 palette copiate dalla
  versione macOS + `windows` dal registro: `AppsUseLightTheme`, `Accent\AccentPalette`),
  `config.rs` (serde + toml, `deny_unknown_fields`, `rustybar check`), `process.rs` (DPI per
  schermo, priorità sotto il normale, EcoQoS, memoria a priorità bassa, un'istanza per cartella),
  `log.rs`, `i18n.rs`.
- `widgets/`: un file per widget, registrato in `widgets::create` e in `KINDS`. Il trait `Widget`
  ha `pill`, `handle` (un handle da aspettare), `signaled` ed `event` (`Signal`: `Foreground`,
  `Resume`, `TimeChanged`, `Shown`); i due ultimi dicono se la pill è cambiata.

## Decisioni (con l'utente, 10/11/2026)

- **Posizione**: barra in alto, registrata come AppBar. La barra delle applicazioni di Windows
  resta com'è (l'utente può metterla a scomparsa da Impostazioni): RustyBar non la tocca.
- **Aspetto**: animazioni con DirectComposition, spente coi giochi; niente vetro.
- **Workspace**: i desktop virtuali di Windows, niente tiling WM.
- **Widget della prima versione**: `clock`, `front_app`, `workspaces`, `system`, `audio` con la
  musica, `network`, `claude`.
- **Schermi**: una barra su ogni schermo; un gioco a schermo intero nasconde solo quella del suo.
- **Lua**: rimandato; prima i widget in Rust.
- **Temi**: `windows` (predefinito, segue chiaro/scuro e accento) più le 24 palette della
  versione macOS.
- **Giochi**: ogni app a schermo intero (anche borderless) più `[games] apps`, eseguibili che
  contano come gioco in primo piano anche in finestra (niente animazioni; timer fermi salvo
  l'orologio, che resta perché la barra si vede).
- **Disegno**: WARP, misurato contro il device GPU (vedi Stato).

## Widget della prima versione

Per ognuno, prima del codice, leggi il widget della versione macOS (`src/widgets/<nome>.rs`, il
doc del modulo è la specifica) e scrivi qui cosa fa su Windows: testi, stati, azioni, fonti dei
dati.

- `clock` (fatto, senza popup): data e ora nei formati di Windows (`GetDateFormatEx` con
  `date`, predefinito `ddd dd/MM` → `Mar 10/11`, prima lettera maiuscola e senza punti;
  `GetTimeFormatEx` senza secondi), due testi nella pill a 10 DIP. Timer waitable a inizio minuto
  con 250 ms di tolleranza (Windows accorpa il risveglio), fermo senza barre visibili, rifatto a
  risveglio, cambio d'ora e di formati. Da fare (dalla versione macOS): popup col calendario che
  scorre i mesi, oggi evidenziato, settimana dal giorno della regione, weekend più tenui; clic su
  un giorno → Calendario.
- `front_app` (fatto): nome dell'app in primo piano da `SetWinEventHook(EVENT_SYSTEM_FOREGROUND)`
  fuori processo, dalla `FileDescription` (poi `ProductName`, poi il nome del file) della versione
  dell'eseguibile, letta una volta per percorso; le app UWP dietro `ApplicationFrameHost.exe`
  (la finestra figlia di un altro processo); taglio a 260 DIP con i puntini. Taskbar e desktop
  lasciano l'app di prima. Icona: non c'è (neanche su macOS).
- `workspaces` (fatto, da provare con più desktop): i desktop virtuali, un numero o il nome del
  desktop, il corrente sotto l'evidenziazione `primary` che scivola. Dal registro
  (`Explorer\VirtualDesktops`: `VirtualDesktopIDs`, `CurrentVirtualDesktop`,
  `Desktops\{id}\Name`) con `RegNotifyChangeKeyValue`. Su questa build (26300) con un solo
  desktop Explorer non scrive né l'elenco né il corrente (ci sono solo 33 voci vecchie di
  sfondi in `Desktops`): la pill si nasconde (`single = true` la tiene). Da provare con l'utente,
  che deve creare un secondo desktop: che i valori compaiano e cambino al cambio di desktop. Il
  clic che cambia desktop non c'è: non ha API pubblica, da scegliere con l'utente fra le
  interfacce COM non documentate (cambiano fra le build) e `SendInput` di Ctrl+Win+←/→.
- `system`: CPU (`GetSystemTimes`) e RAM (`GlobalMemoryStatusEx`) su un timer lento, ridisegno
  solo se il numero mostrato cambia. GPU: niente PDH `GPU Engine` (enumera i processi, costa);
  da studiare `D3DKMTQueryStatistics` per nodo. Temperature solo con le API del produttore
  (NVAPI/ADLX): da decidere.
- `audio`: volume e dispositivi da Core Audio (`IMMNotificationClient`,
  `IAudioEndpointVolumeCallback`); musica da GSMTC, con gli eventi di sessione, traccia e
  riproduzione.
- `network`: stato da `NotifyIpInterfaceChange` / `INetworkListManager`, Wi-Fi da
  `WlanRegisterNotification`; contatori (`GetIfEntry2`) solo a popup aperto.
- `claude`: come su macOS, i numeri vengono solo da Claude Code (`claude auth status --json` e
  `claude -p` con la richiesta `get_usage`, senza mandare messaggi). È l'unico processo esterno
  previsto, ed è pesante (Claude Code gira su Node): ogni 5 minuti solo con la barra visibile, mai
  con un gioco a schermo intero, all'apertura del popup se i numeri hanno più di un minuto; figlio
  senza finestra (`CREATE_NO_WINDOW`) a priorità idle con EcoQoS. Costo da misurare e da scrivere
  nel README.

## Domande aperte (da decidere con l'utente)

- Cambio di desktop col clic su `workspaces`: COM non documentato o `SendInput` (vedi sopra).

## Stato

- 10/11/2026: repo creata in `E:\Developer\rustybar-windows` (ramo `main`) con questo CLAUDE.md e
  la guardia sulle attribuzioni (config locale, `.claude/settings.json`, hook), provata con
  `git commit` veri in una repo usa e getta. Prime decisioni con l'utente: posizione, aspetto,
  workspace, widget della prima versione (sopra).
- 10/11/2026, **M1**: motore (processo, config che si ricarica, temi, WARP + DirectComposition,
  una barra per schermo come AppBar, schermo intero, giochi della lista, ripresa dopo un errore
  del device) e `clock`, `front_app`, `workspaces` senza popup né clic. Schermi qui: 5120×1440
  (principale) e 1920×1080 sotto, 100%. Misurata con la barra di prova (`reserve = false`), a
  riposo per 60 s: 5,3 MB privati (23 MB di working set con le DLL condivise), 0 ms di CPU,
  ~0,7 risvegli/s (lo scatto del minuto), GPU 0% e 0 MB, 9 thread (i worker di WARP, fermi),
  0 figli, primo frame 36–41 ms, binario ~620 KB. Col device GPU (`RUSTYBAR_GPU=hardware`):
  37 MB privati, 9,5 MB di VRAM, 24 thread, 1041 handle, primo frame 121 ms: per questo WARP.
  Provati con una barra di prova che riservava lo spazio: area di lavoro ridotta di 40 px su
  entrambi gli schermi e ripristinata all'uscita; una finestra senza bordi sullo schermo
  principale nasconde solo la sua barra (`ABN_FULLSCREENAPP` arriva) e la barra torna quando si
  chiude; WoW vero a schermo intero borderless in primo piano nasconde la barra principale e
  lascia l'altra. Primo frame con l'AppBar: 296 ms prima di spostare la registrazione dopo il
  primo frame, 41 ms dopo. Non visto a occhio: lo scivolamento (pill e evidenziazione), il
  cambio di tema chiaro/scuro e di accento al volo, il cambio di DPI. Da fare: popup e clic
  (prima il calendario di `clock`), poi `system`, `audio`, `network`, `claude`; installazione e
  avvio con Windows.
