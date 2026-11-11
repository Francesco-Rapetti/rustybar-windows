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
  contro quello hardware, costa un settimo della RAM (vedi Stato). WARP rasterizza sul thread
  chiamante (`D3D11_CREATE_DEVICE_PREVENT_INTERNAL_THREADING_OPTIMIZATIONS`), senza il suo pool
  di thread: per superfici così piccole il pool aggiungeva solo thread da svegliare a ogni
  ridisegno (misurato: da 11 a 2–5 thread, da 6,3 a 1,8 risvegli/s). Animazioni spente con un
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
- La barra vera è installata (`rustybar install`, sotto): gira da
  `%LOCALAPPDATA%\Programs\RustyBar\rustybar.exe` con la config di `%APPDATA%\rustybar`, e parte
  con Windows. Per provarne una nuova build: `target\release\rustybar.exe install` (chiude quella
  installata, la sostituisce e la riavvia); l'utente può star giocando, quindi chiedere prima.
- **Le sessioni dell'app Claude girano dentro il suo pacchetto MSIX**, e Windows tiene per il
  pacchetto parte di quello che i loro processi scrivono: il registro `HKCU\Software` sempre
  (anche senza identità di pacchetto: `GetCurrentPackageFullName` dice 15700 e una chiave scritta
  da qui da fuori non si vede), le cartelle nuove sotto `%APPDATA%`/`%LOCALAPPDATA%` a volte
  (finiscono in `%LOCALAPPDATA%\Packages\Claude_pzs8sxrjxfjjc\LocalCache\…`). Da qui si vede la
  fusione delle due cose, quindi sembra tutto a posto anche quando non lo è: per sapere cosa vede
  il PC vero bisogna guardare da un processo avviato da Esplora risorse (`explorer.exe <file>`:
  uno script `.vbs` che scrive il risultato in un file della cartella utente, poi cancellati). Le
  scritture in `%TEMP%` e nelle cartelle che esistono già vanno nel posto vero. Per questo
  `rustybar install` fa sempre il lavoro in un processo avviato da Esplora risorse, e i processi
  avviati da qui (barre di prova comprese) muoiono con la sessione se restano nel suo job.
- Barra di prova accanto a quella vera: una cartella con un `rustybar.toml` con
  `[bar] reserve = false` (non sposta le finestre), poi `RUSTYBAR_CONFIG_DIR=<cartella>` e
  `target\release\rustybar.exe`; con la stessa variabile `rustybar quit` chiude lei sola (le
  variabili d'ambiente di PowerShell non restano fra un comando e l'altro: rimetterla ogni volta).
  Log in `%LOCALAPPDATA%\rustybar\rustybar-<nome cartella>.log` (quella vera: `rustybar.log`), col
  tempo del primo frame.
- `scripts\measure.ps1 [-Id <pid>] [-Seconds 30]`: memoria privata, CPU, risvegli (cambi di
  contesto dei thread), GPU (motori e memoria), thread, handle, figli e con cosa disegna (WARP o
  driver GPU). Usa le classi WMI delle prestazioni, che hanno lo stesso nome in ogni lingua
  (`Get-Counter` no: qui Windows è in italiano). Dopo una raffica di popup aspettare un minuto:
  i worker del pool di thread di Windows nati nel frattempo escono da soli e falsano i risvegli.
- Guidare la barra di prova senza mouse: `rustybar msg popup <widget>` (con la stessa
  `RUSTYBAR_CONFIG_DIR`) apre e chiude un popup, `rustybar msg query` dice dove sono le pill in
  pixel dello schermo (dove fotografare). Per il resto `PostMessage` di `WM_LBUTTONDOWN` alla
  finestra `RustyBarBar` (lParam = y << 16 | x, coordinate client) e di `WM_KEYDOWN` alla
  `RustyBarPopup`. Un popup aperto così non prende il primo piano (Windows lo rifiuta, lo dice il
  log) e si chiude da solo quando un'altra app viene davanti. Ogni comando PowerShell è un
  processo nuovo: le `Add-Type` vanno in uno script da includere ogni volta; uno script `.ps1`
  lanciato da Git Bash vuole `powershell -ExecutionPolicy Bypass -File`. In Git Bash
  `rustybar check`/`msg` aspettano e stampano (in PowerShell un eseguibile GUI non si aspetta:
  `| Out-Host`). Qui non c'è Python.
- `RUSTYBAR_GPU=hardware`: device Direct3D hardware invece di WARP, per confrontare.
- Screenshot della barra: `System.Drawing` `CopyFromScreen` sulla striscia in alto, poi guardarli.

## Com'è fatta

- `app.rs`: tutto sul main thread. Il ciclo dorme in `MsgWaitForMultipleObjectsEx` su: messaggi,
  i handle dei widget (timer, registro) e la cartella della config. Window procedure e hook
  mettono solo un `Event` in coda; il ciclo la svuota dopo il dispatch, così niente rientra
  nell'app mentre lavora. Una finestra nascosta (`RustyBarControl`, titolo = cartella della
  config) riceve i broadcast (schermi, tema, ora, risveglio, `TaskbarCreated`, `rustybar quit`)
  e i comandi di `rustybar msg` (`ipc.rs`: `WM_COPYDATA` con la firma `RBM1`; la risposta torna
  con un altro `WM_COPYDATA` a una finestra message-only di classe `RustyBarMsg` del processo
  `msg`, che aspetta fino a 5 s; niente pipe né socket da tenere in ascolto).
  Barra nascosta su ogni schermo → device rilasciati (`Gfx` a `None`), si rifanno quando una barra
  torna. Animazioni solo se il tema le vuole, Windows le ha accese, nessun gioco della lista in
  primo piano e tutte le barre visibili.
- `bar.rs`: una barra per schermo (`WS_EX_NOREDIRECTIONBITMAP`, topmost, senza attivazione),
  AppBar registrata dopo il primo frame (Explorer ci mette ~200 ms a spostare le finestre),
  `fullscreen_here` (finestra in primo piano che copre lo schermo senza barra del titolo, o
  `QUNS_RUNNING_D3D_FULL_SCREEN`). Per pill un visual con lo sfondo come contenuto (nessuna
  superficie per una pill `bare`), l'evidenziazione e il testo in visual figli; firme per
  ridisegnare solo ciò che cambia; scivolamenti con `Gfx::slide` (cubica ease-out eseguita da
  DWM).
- Clic e popup: la barra (`WS_EX_NOACTIVATE`, `MA_NOACTIVATE`) manda `WM_LBUTTONDOWN` come
  `Event::Click`; `Bar::hit` trova la pill e l'elemento della pill dalla sola x (vale tutta
  l'altezza, fino al bordo dello schermo; ogni elemento con metà dello spazio ai lati). Una pill
  senza popup riceve il clic con `Widget::click(elemento)` (i desktop di `workspaces`). Un popup
  alla volta (`popup.rs`): una finestra `WS_EX_NOREDIRECTIONBITMAP`
  topmost creata all'apertura e distrutta alla chiusura, con un visual radice che ha per
  contenuto la scena e un visual figlio per la tinta dell'hover (il colore del testo all'8%),
  che si sposta di zona in zona senza ridisegnare nulla. Il popup prende il primo piano
  (`SetForegroundWindow`, permesso perché il processo ha appena ricevuto il clic) e si chiude a
  `WM_ACTIVATE`/`WA_INACTIVE` o quando l'hook vede un'altra app in primo piano; Esc, Alt+F4 e la
  pill lo chiudono restituendo il primo piano alla finestra di prima (`Popup::previous`, solo se
  il popup è ancora davanti). Apertura: scende di 24 DIP in 0,25 s (cubica ease-out) e appare in
  0,12 s; chiusura: dissolvenza di 0,1 s, finestra disabilitata e distrutta allo scadere di un
  `SetTimer` (`PopupGone`). Si allinea alla pill dal lato del suo gruppo (sinistra, centro,
  destra), a `popup_gap` sotto la barra, dentro lo schermo a `margin` dai bordi. Si chiude anche
  se la sua barra si nasconde, coi device rilasciati, al cambio di schermi, DPI e config.
  Il widget descrive il popup come `ui::Scene` (rettangoli arrotondati, testi, zone con
  un'azione) in DIP; la firma della scena evita di ridisegnarla uguale. Trait: `has_popup`,
  `popup_shown` (crea o butta ciò che serve solo al popup), `popup`, `popup_input`
  (`Action`, `Wheel`, `Key` → `Reply`: `Nothing`, `Redraw`, `Close`, `Opened`).
- `gfx.rs` (D3D11 WARP, D2D, DirectComposition, `draw` in DIP alla scala dello schermo), `ui.rs`
  (`Pill` di `Item` di testo, misurati con DirectWrite: `small`, `fixed` = largo almeno quanto un
  testo modello e allineato a sinistra, `space` = spazio prima dell'item; `Pill::bare` = senza
  sfondo né bordo, stessa misura e stessa area del clic, anche nel popup ⋯; `Scene` e `Fonts` dei
  popup, `Backdrop` per i colori propri di un popup), `palette.rs` (i colori di un'immagine in
  Oklab, per il popup di `media`), `theme.rs` (le 24 palette copiate dalla versione macOS + `windows` dal registro:
  `AppsUseLightTheme`, `Accent\AccentPalette`; `level` per giallo e rosso, `tile` per i riquadri
  dei popup), `config.rs` (serde + toml, `deny_unknown_fields`, `rustybar check`,
  `rustybar.local.toml` fuso sopra con `merge`), `process.rs`
  (DPI per schermo, priorità sotto il normale, EcoQoS, memoria a priorità bassa, un'istanza per
  cartella; `make_small` anche per il figlio di `claude`; `quit` di una cartella con attesa),
  `install.rs` (`rustybar install`/`uninstall`), `stats.rs` (CPU, memoria, GPU dal
  kernel grafico, dischi, processi: le letture di `system`), `json.rs` (lettore JSON per le
  risposte di Claude Code), `log.rs`, `i18n.rs` (testi, numeri e misure col separatore della
  regione).
- L'app tiene le pill misurate per nome (`App::measured`): una pill uguale all'ultima riusa i suoi
  layout, così un numero che cambia rimisura solo la pill del suo widget.
- La barra che non ci sta (`fit.rs`, `App::fit_bars`, `fit_bar`; come su macOS senza notch, isola
  né forme «slim»): ogni barra si adatta da sola (`Bar::fit`: livelli dell'ultimo adattamento,
  centro compatto, widget compatti e nascosti per lato). Se tutto entra in forma piena (il caso
  normale qui) costa una somma e non si misura altro; altrimenti si misurano le pill compatte
  (`Widget::compact`, tenute solo se più strette, in cache come `nome\0compact`) e le pill ⋯.
  Ordine: prima il gruppo centrale compatto se i lati non entrano accanto a quello pieno; poi i
  widget più lontani dal loro bordo passano uno alla volta alla forma compatta (a pari distanza
  prima il lato destro), poi, tutti compatti, vanno uno alla volta nel popup del ⋯ del loro lato
  (`⋯left`, `⋯right`, icona `E712`, all'estremità interna del lato). Le forme più grandi tornano
  solo con 24 DIP di margine (`SLACK`). Il popup ⋯ lo disegna l'app (`more_scene`,
  `pill_shapes`): le pill nascoste nella forma piena una sotto l'altra, allineate al lato; clic su
  una con popup → il suo popup sotto il ⋯ (`Bar::pill_span` di un widget nascosto dà quella del
  suo ⋯); su una senza popup che fa qualcosa (`Widget::clickable`: `workspaces`) → il clic
  sull'elemento, come nella barra. `msg query` dice `compact:` e `in ⋯left:`; `msg popup ⋯right`
  apre il popup. Forme compatte: `clock` l'ora, `front_app` a 120 DIP, `media` il titolo a 110,
  `network` le icone, `audio` le icone (uscita e batteria), `system` la RAM, `claude` la
  sessione.
- COM: il thread principale è un apartment STA (`CoInitializeEx` in `app::run`, il ciclo ne
  pompa i messaggi). I callback di Core Audio e i gestori WinRT arrivano su thread loro: toccano
  solo atomici, mutex ed eventi (`wake.rs`: `Wake`, un evento auto-reset condiviso con `Arc`;
  `Timer`, un timer waitable che gira solo quando serve). Mai `join()` di un'operazione WinRT sul
  thread principale: si blocca (provato); si usa `when`. `windows-core` è una dipendenza
  diretta solo perché le macro `#[implement]` e `#[interface]` lo nominano.
- `widgets/`: un file per widget, registrato in `widgets::create` e in `KINDS`. Il trait `Widget`
  ha `pill`, `handle` (un handle da aspettare) o `handles` (più d'uno: un evento e un timer) con
  `signaled`/`signaled_at`, ed `event` (`Signal`: `Foreground`, `Resume`, `TimeChanged`,
  `Shown`, `Game` = un gioco della lista in primo piano); dicono se ciò che il widget mostra è
  cambiato (la pill, o il popup aperto). Nella pill `Item::icon` (Segoe Fluent Icons); nei popup
  `Shape::Image`, `Scene::slider`, `Input::Slide`/`Release`, testi tagliati coi puntini.

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
- **Popup**: il clic sulla pill non ruba il primo piano; il popup sì, come i riquadri della
  taskbar di Windows: si chiude quando perde il primo piano (clic altrove, senza hook), con Esc o
  con un nuovo clic sulla pill (con Esc e la pill il primo piano torna all'app di prima). Stile
  Windows 11: angoli di 8, bordo di 1 px, scivolamento dall'alto e dissolvenza eseguiti da DWM.
- **Calendario di `clock`**: un mese alla volta, ‹ › e rotella, «Oggi»; clic su un giorno →
  Google Calendar su quel giorno nel browser (opzione `open`, un URL con `{year}` `{month}`
  `{day}`).

## Decisioni (con l'utente, 10–11/11/2026, sui widget)

- **`system`**: pill con CPU, GPU (solo i motori 3D) con la temperatura e RAM, ogni 5 s; popup
  completo (grafici, memoria e VRAM, dischi, app che consumano di più).
- **`audio` e `media`**: due widget; niente rotella sul volume; popup con volumi, cambio di
  dispositivo (`IPolicyConfig`), player con copertina; batteria Bluetooth anche nella pill.
- **`network`**: icona e nome; popup con connessione, download e upload, dettagli, ping; niente
  IP pubblico (nessuna richiesta verso l'esterno); Wi-Fi anche con elenco e connessione.
- **`claude`**: un account in `~/.claude` (altri come istanze del widget), ogni 5 minuti, pill con
  l'ora d'azzeramento; fuori dal layout predefinito perché costa un processo.
- **`workspaces`**: il clic cambia desktop con `SendInput` di Ctrl+Win+←/→.

## Widget della prima versione

Per ognuno, prima del codice, leggi il widget della versione macOS (`src/widgets/<nome>.rs`, il
doc del modulo è la specifica) e scrivi qui cosa fa su Windows: testi, stati, azioni, fonti dei
dati.

- `clock` (fatto): data e ora nei formati di Windows (`GetDateFormatEx` con
  `date`, predefinito `ddd dd/MM` → `Mar 10/11`, prima lettera maiuscola e senza punti;
  `GetTimeFormatEx` senza secondi), due testi nella pill a 10 DIP. Timer waitable a inizio minuto
  con 250 ms di tolleranza (Windows accorpa il risveglio), fermo senza barre visibili, rifatto a
  risveglio, cambio d'ora e di formati. Popup col calendario (260×292 DIP), un mese alla volta:
  in alto la data lunga della regione (`DATE_LONGDATE`, `Martedì 10 novembre 2026`) e «Oggi»
  nel colore `primary`; sotto ‹ `Novembre 2026` › (chevron di Segoe Fluent Icons, titolo da
  `MMMM yyyy`); i giorni della settimana coi nomi più corti della regione
  (`LOCALE_SSHORTESTDAYNAME1..7`, `lu ma me…`) dal primo giorno della regione
  (`LOCALE_IFIRSTDAYOFWEEK`); sempre sei settimane, così il popup non cambia misura; oggi nel
  tondo `primary` col testo `on_primary`, sabato e domenica in `overlay`. Mesi con ‹ ›, la
  rotella (in su = indietro), ←/→, ↑/↓, PagSu/PagGiù; Home e «Oggi» tornano al mese corrente.
  Clic su un giorno → `open` con `{year}` `{month}` `{day}` (predefinito Google Calendar, vista
  del giorno; `""` toglie il clic) con `ShellExecuteW` dopo `AllowSetForegroundWindow`: la pagina
  prende il primo piano e il popup si chiude senza restituirlo. Mese mostrato, primo giorno e
  nomi letti all'apertura e buttati alla chiusura; a mezzanotte, col popup aperto, oggi si sposta
  (lo scatto del minuto ridisegna il popup, che si disegna solo se la scena è cambiata).
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
  che deve creare un secondo desktop: che i valori compaiano e cambino al cambio di desktop. Clic
  su un desktop (deciso con l'utente l'11/11/2026, fra questo e le interfacce COM non documentate
  che cambiano fra le build): `SendInput` di Ctrl+Win+← o → tante volte quanti sono i desktop in
  mezzo, in una sola chiamata (i tasti dell'utente non si infilano in mezzo), le frecce con
  `KEYEVENTF_EXTENDEDKEY`; niente con un gioco della lista in primo piano (tasti simulati: un
  anti-cheat potrebbe notarli); il registro dice poi del cambio e l'evidenziazione scivola. Non
  provato (cambierebbe desktop; qui ce n'è uno).
- `system` (fatto, confermato con l'utente): pill `CPU 12%  GPU 34% 62°  RAM 43%`, etichette
  piccole in `subtext`, ogni numero largo quanto `100%` (`Item::fixed`: la pill non cambia
  misura, si ridisegna solo il testo); giallo da `warning` (75), rosso da `critical` (90); la
  temperatura gialla da 10° sotto quella che il driver dice alta (`TemperatureWarning` delle
  `ADAPTERPERFDATA_CAPS`), rossa da lì. Fonti (`stats.rs`), tutte in-process e senza WMI né
  PDH: CPU da `GetSystemTimes` (due letture), RAM da `GlobalMemoryStatusEx`, GPU dal kernel
  grafico: `D3DKMTEnumAdapters2`, la scheda non software con più memoria dedicata;
  `D3DKMTQueryStatistics` dei soli motori 3D (`NodeMetadata` con `EngineType` 3D; ogni motore
  costa ~0,3 ms di CPU nel driver della Radeon e sono 15: leggerli tutti, come il Task Manager,
  costerebbe 4 ms a lettura); temperatura da `KMTQAITYPE_ADAPTERPERFDATA` (come il Task
  Manager, pochi µs); VRAM dai segmenti non aperture. Timer waitable periodico (`interval`, 5 s,
  un decimo di tolleranza per accorpare i risvegli), fermo senza barre visibili e con un gioco
  della lista in primo piano (numeri fermi, in grigio); dopo uno stop o un risveglio riparte da
  letture nuove dopo 0,5 s. Popup a riquadri (344 DIP), ogni 2 s mentre è aperto: CPU e GPU
  affiancati (valore, core fisici / temperatura, le ultime 30 letture a barre, tenute dall'avvio
  così il grafico è pieno all'apertura); memoria (in uso di totale, impegnata di limite); memoria
  video (usata di dedicata, nome della GPU); un riquadro per disco fisso
  (`GetLogicalDriveStrings` + `GetDriveType` = fisso, `GetDiskFreeSpaceEx`, etichetta da
  `GetVolumeInformation`, «Disco locale» senza; giallo da `disk_warning` 90%, rosso da
  `disk_critical` 95%; clic → il disco in Esplora file; letti all'apertura e ogni 10 s); le 5 app
  che consumano di più, CPU (sopra 0,05%) o memoria privata con un selettore, raggruppate per
  eseguibile, col nome della versione come `front_app` (un processo che non si apre, come
  `dwm.exe`: il file omonimo in System32), sopra la riga Sistema/Utente/Inattivo
  (`NtQuerySystemInformation(SystemProcessInformation)`, una chiamata da ~5 ms per tutti i
  processi e i thread, solo a popup aperto; la CPU dalla seconda lettura, prima righe vuote);
  «Apri Gestione attività» (`taskmgr.exe`) e «Impostazioni di archiviazione…»
  (`ms-settings:storagesense`). Opzioni: `interval`, `warning`, `critical`, `disk_warning`,
  `disk_critical`, `gpu = false` (niente GPU). Fuori perché pesano o non hanno API: GPU per
  processo (un handle per processo × motori × 0,3 ms), temperatura della CPU (solo WMI o un
  driver), icone delle app.
- `audio` (fatto, confermato con l'utente: due widget, `audio` e `media`; niente rotella;
  batteria Bluetooth anche nella pill): pill con l'icona dell'uscita (per tipo: cuffie, cuffie
  con microfono, schermo HDMI/DP, altoparlanti per livello; barrata se muta) e il volume
  (`fixed("100%")`, grigio se muto); con un'uscita Bluetooth la batteria (icona a decimi di
  Segoe Fluent Icons, gialla al 20%, rossa al 10%). Popup (320 DIP): un riquadro per l'uscita e
  uno per l'ingresso (icona = muto con un clic, nome, percentuale, cursore stile Windows 11 che
  si clicca o si trascina: `Zone::slider`, `Input::Slide`, cattura del mouse durante il
  trascinamento; alzarlo toglie il muto), poi «Uscita» e «Ingresso»: i dispositivi attivi in
  ordine di nome, il predefinito col pallino `primary` e in grassetto, la batteria se Bluetooth;
  clic = predefinito per i tre ruoli con `IPolicyConfig` (interfaccia COM non documentata, la
  stessa delle impostazioni audio, di EarTrumpet e SoundSwitch; `#[interface]` col solo
  `SetDefaultEndpoint` chiamato); «Impostazioni audio» (`ms-settings:sound`) e «Mixer volume»
  (`ms-settings:apps-volume`). Fonti: Core Audio (`IMMDeviceEnumerator`, ruolo `eConsole`;
  nome `PKEY_Device_FriendlyName`, tipo `PKEY_AudioEndpoint_FormFactor`, Bluetooth se
  `PKEY_Device_EnumeratorName` comincia con `BTH`); `IMMNotificationClient` (dispositivi e
  predefiniti; dei cambi di proprietà solo il nome) e `IAudioEndpointVolumeCallback` sui due
  predefiniti: i callback (`#[implement]`, su thread di Core Audio) mettono un flag e svegliano il
  ciclo con un evento (`wake::Wake`); il widget rilegge sul thread principale. Batteria: nodo
  dell'endpoint `SWD\MMDEVAPI\<id>` → `DEVPKEY_Device_ContainerId` → i nodi presenti di
  `BTHENUM`/`BTHHFENUM`/`BTHLE`/`BTHLEDEVICE` dello stesso contenitore → la proprietà
  `{104EA319-6EE2-4701-BD47-8DDBF425BBE5} 2` (un byte, quella di Impostazioni), con
  Configuration Manager; Windows non avvisa quando cambia: riletta al cambio di dispositivo,
  all'apertura del popup e ogni 5 minuti (`wake::Timer`) solo con un dispositivo Bluetooth in
  uso, una barra visibile e nessun gioco della lista. Senza barre visibili tutti i callback sono
  tolti (zero risvegli con un gioco a schermo intero) e rimessi, con tutto riletto, quando una
  barra torna.
- `media` (fatto): la musica da GSMTC, pill `♪ Titolo  Artista` (titolo tagliato a 240 DIP,
  artista in `subtext` a 160), in pausa grigia col simbolo di pausa (`paused = false` la
  nasconde), nascosta senza sessione o col titolo vuoto. Popup (320 DIP): copertina 76 DIP con
  gli angoli arrotondati (`Shape::Image`, pennello bitmap lineare: l'interpolazione cubica fa
  compilare uno shader a Direct2D, `d3dcompiler_47.dll`, +2 MB che restavano), titolo, artista,
  app (dall'AUMID reso leggibile: `Spotify.exe` → «Spotify», `chrome` → «Chrome», dopo il `!`
  per le app pacchettizzate, niente se è un codice come quello di Firefox; il nome di Start
  dalla cartella App costava 54 ms e 1,2 MB che restavano), barra della posizione (clic o
  trascinamento; lo spostamento parte quando si lascia, `Input::Release`) coi tempi, ⏮ ⏯ ⏭
  (grigi e senza zona se l'app non li permette), spazio = play/pausa. Fonti:
  `GlobalSystemMediaTransportControlsSessionManager::RequestAsync` asincrono (il primo frame non
  aspetta), `CurrentSessionChanged`, sulla sessione `MediaPropertiesChanged` (il gestore chiede
  lui le proprietà, asincrone) e `PlaybackInfoChanged`; `TimelinePropertiesChanged` solo a
  popup aperto. I gestori girano su thread del pool: mettono flag e dati in `Shared` (con un
  numero di sessione per buttare le risposte vecchie) e svegliano il ciclo. La posizione si
  calcola da quella detta dall'app più il tempo da `LastUpdatedTime`; un timer di 1 s solo a
  popup aperto e in riproduzione. Copertina solo per il popup aperto (`cover = false` la toglie):
  `Thumbnail().OpenReadAsync()` sul thread del pool dove arrivano le proprietà (sul thread STA
  `join` si blocca), WIC: ritaglio quadrato al centro, scala a 152 px, PBGRA; buttata alla
  chiusura. Senza barre visibili tutti i gestori sono tolti.
  Extra (dalla versione macOS, 11/11/2026): **scelta fra le app** con più sessioni (`GetSessions`,
  `SessionsChanged` solo a popup aperto): in alto un selettore a segmenti col nome dell'app
  (l'AUMID reso leggibile; per un AUMID che è un codice il titolo della sua sessione, chiesto
  asincrono), quella seguita in `primary`; il clic segue quella sessione (anche la pill, finché
  il popup è aperto); alla chiusura si torna alla corrente di Windows. **Shuffle e ripetizione**
  ai lati di ⏮ ⏯ ⏭, solo se l'app li permette (`IsShuffleEnabled`, `IsRepeatEnabled`; stato da
  `IsShuffleActive` e `AutoRepeatMode`), accesi nel colore `primary` su un tondo; ripetizione:
  niente → tutta la lista → un brano (icona `E8ED`) → niente; il cambio si vede subito e la
  risposta dell'app lo corregge. **Volume dell'app** come nel mixer: le sue sessioni audio
  (`IAudioSessionManager2` su ogni uscita attiva, non di sistema né scadute) trovate per nome
  dell'eseguibile (`Spotify.exe`, `chrome` → `chrome.exe`, `MSEdge` → `msedge.exe`) o per AUMID
  con `GetApplicationUserModelId` per le app pacchettizzate; per un AUMID che è un codice
  (Firefox) niente; riga con l'altoparlante (clic = muto), cursore e percentuale; impostate tutte
  insieme, lette dalla prima; cercate all'apertura e al cambio d'app (5–6 ms sul thread
  principale), seguite con `IAudioSessionEvents` fino alla chiusura. **Colori dalla copertina**
  (`palette.rs`, quello della versione macOS sui pixel BGRA di WIC, un pixel ogni due): sfondo dal
  colore più presente, accento dal più vivo con una buona parte, alla luminosità del tema chiaro
  o scuro; ~3 ms sul thread del pool dopo la decodifica; il popup li usa con `Scene::backdrop`
  (sfondo, bordo e tinta dell'hover suoi); `cover_colors = false` tiene quelli del tema.
  `examples/fake_player.rs`: un player finto (sessione GSMTC con `MediaPlayer` e SMTC, brani e
  copertine inventati e disegnati lì, un WAV muto in loop per avere una sessione audio, comandi,
  shuffle, ripetizione e posizione che funzionano; esce da solo); una copia con un altro nome
  (`Aurora.exe`) è un'altra app. Le feature `Media_Core`, `Media_Playback`, `Storage` sono solo
  nelle dev-dependencies. La miniatura va da `StorageFile`: un URI `file://` non è accettato.
- `network` (fatto, confermato con l'utente: icona e nome; popup con connessione, download e
  upload, dettagli e ping; niente IP pubblico; Wi-Fi anche con elenco e connessione): pill con
  l'icona (Ethernet, Wi-Fi a tacche per il segnale; Ethernet con l'avviso o Wi-Fi gialli e
  «Nessun Internet» con la sola rete locale; Ethernet con la croce o il globo barrato rossi e
  «Disconnesso» senza rete) e il nome (il nome della scheda, `Ethernet`; sul Wi-Fi l'SSID, o
  «Wi-Fi» senza il permesso di posizione), lucchetto e «VPN» davanti con una VPN su (scheda su
  di tipo PPP 23, tunnel 131 o virtuale 53 con un IPv4). Popup (344 DIP, riquadri come `system`):
  connessione (stato in verde, giallo o rosso, nome, scheda · velocità del collegamento; sul
  Wi-Fi segnale · banda · generazione · velocità); download e upload (velocità ora, gli ultimi
  30 secondi a barre scalate sul secondo più carico, almeno 64 KB/s; il totale della scheda);
  la VPN (nome, descrizione, IP); le reti Wi-Fi (↻ per una scansione, fino a 8 righe: la
  connessa, poi le conosciute, poi per segnale; tacche, nome, lucchetto, banda; clic = collegarsi
  col profilo, o con un profilo temporaneo per le reti aperte; una rete protetta nuova apre
  l'elenco di Windows, `ms-availablenetworks:`, che chiede la password; «Altre reti…»; senza il
  permesso di posizione il motivo e «Impostazioni di posizione…»); dettagli (IP locale col
  prefisso, router col ping, DNS con gli IPv4 prima, MAC, IPv6 globale; un clic copia negli
  appunti e il riquadro dice «Copiato» per 2 s); «Impostazioni di rete». Fonti: IP Helper
  (`GetNetworkConnectivityHint` e `NotifyNetworkConnectivityHintChange` per il livello,
  `NotifyIpInterfaceChange` e `NotifyUnicastIpAddressChange` per schede e indirizzi; i callback
  mettono un flag e svegliano il ciclo), `GetAdaptersAddresses` sul thread principale a ogni
  cambio (la scheda su con un router e la metrica più bassa, altrimenti una su senza router);
  WLAN (`WlanOpenHandle`, che fallisce senza il servizio: un PC senza Wi-Fi non ha altro;
  `WlanQueryInterface` per la connessione, `WlanGetNetworkBssList` per la banda dalla frequenza,
  `WlanGetAvailableNetworkList`, `WlanScan` all'apertura e con ↻, `WlanConnect`,
  `WlanRegisterNotification` per connessione, segnale e scansione finita). Solo a popup aperto:
  `GetIfEntry2` ogni secondo (timer), un ping al router ogni 5 s con `IcmpSendEcho2` asincrono
  su un evento (risposta letta quando l'evento scatta; il buffer è del widget, così una risposta
  tardiva non scrive in memoria liberata). Senza barre visibili tutte le notifiche sono
  cancellate (`CancelMibChangeNotify2`, `WlanRegisterNotification` a `NONE`).
- `claude` (fatto, confermato con l'utente: un account in `~/.claude`, ogni 5 minuti, pill con
  l'ora d'azzeramento): pill `✻ 42% · 31% · 17:07` (la stellina nel colore dell'account,
  `color`, arancio di Claude; sessione di 5 ore e settimana, gialle da `warning` 75 e rosse da
  `critical` 90; tutto grigio se l'ultima lettura è fallita; l'ora in cui riparte la sessione);
  senza account `✻ Accedi`, prima della prima lettura o senza Claude Code `✻ –`. Un altro account
  è un'altra istanza del widget: `[widgets.claude_lavoro] type = "claude"`, `dir` (la cartella di
  config di Claude Code, `CLAUDE_CONFIG_DIR`) e `color`. Popup (344 DIP): intestazione con la
  stellina in un tondo, il nome (da `oauthAccount.displayName` di `.claude.json`, o l'email), il
  piano in un'etichetta, email e organizzazione (non quella personale, che porta l'email nel
  nome), l'età dei numeri (`adesso`, `3 min fa`; gialla se l'ultima lettura è fallita;
  `Aggiornamento…` mentre legge) e ↻; due riquadri, sessione e settimana: percentuale, barra,
  `Si azzera alle 15:10` (`lun 01:00` oltre un giorno) e `tra 4 h 34 min` (`al prossimo
  messaggio` prima che parta), aggiornati ogni 30 s a popup aperto; «Utilizzo su claude.ai» e
  «Esci» (`claude auth logout`, al secondo clic). Senza account: «Nessun account», la cartella di
  config e «Accedi» (`claude auth login`: si apre il browser; «Accedi nel browser…» mentre
  aspetta, «Accesso non riuscito» con l'output in `claude-login.log` accanto al log). Un accesso
  senza limiti del piano (chiave API) e un PC senza Claude Code lo dicono in un riquadro.
  Fonti: Claude Code, l'unica via: `claude -p --safe-mode --strict-mcp-config
  --no-session-persistence --input-format stream-json --output-format stream-json --verbose` con
  la richiesta di controllo `get_usage` su stdin (i dati di `/usage`, senza mandare messaggi né
  usare il piano), `claude auth status --json` solo all'avvio, all'apertura del popup e dopo una
  lettura fallita (`json.rs`, un lettore JSON piccolo, al posto di una dipendenza). `claude.exe`:
  `path`, il PATH, `~\.local\bin`, altrimenti il più recente che porta l'app desktop Claude
  (`%LOCALAPPDATA%\Claude-Data\*\claude-code\<versione>\<build>\claude.exe`): qui non c'è una
  CLI installata a parte. La CLI dell'app non ha login suo (l'app passa il proprio solo ai
  Claude Code che avvia), quindi va collegata una volta con «Accedi». Il figlio: senza finestra,
  priorità idle, EcoQoS e memoria a priorità bassa (`process::make_small`), in un Job object con
  `KILL_ON_JOB_CLOSE` (muore con la barra), senza le variabili `CLAUDE_*`/`ANTHROPIC_*` che un
  Claude Code genitore passa (sono una trentina e cambiano il comportamento della CLI) e con
  `DISABLE_AUTOUPDATER`, `DISABLE_TELEMETRY`, `DISABLE_ERROR_REPORTING`; lo lancia un thread di
  lavoro che legge le pipe mentre gira, aspetta l'uscita con un timeout (20 s lo stato, 30 s i
  numeri, 5 minuti l'accesso) e poi sveglia il ciclo. Timer di 5 minuti (primo a 3 s
  dall'avvio) solo con una barra visibile, nessun gioco della lista e un account da leggere:
  senza account o senza Claude Code non parte niente fino all'apertura del popup.

## Dalla versione macOS (deciso con l'utente l'11/11/2026)

Confrontati i widget e le funzioni della versione macOS con quelli di qui. Scelti dall'utente,
in quest'ordine (le piccole prima, Lua e impostazioni per ultime):

1. `rustybar msg`, `rustybar.local.toml`, `timezone` di `clock` (fatti, sotto).
2. `power`: il menu di Windows (l'equivalente di `apple`) (fatto).
3. Extra del popup di `media`: scelta fra le app, shuffle e ripetizione, volume dell'app, colori
   dalla copertina (fatti).
4. La barra che non ci sta: forme compatte, poi un pill ⋯ coi widget che non entrano (oggi le
   pill si sovrappongono) (fatto).
5. `home` (Home Assistant), 6. `events` (prossimo evento dall'indirizzo iCal segreto di Google
   Calendar: Windows non ha un calendario leggibile da un'app non pacchettizzata), 7. `docker`
   (Engine API sulla named pipe di Docker Desktop) (fatti).
8. Dati finti (`RUSTYBAR_MOCK`) e screenshot dei popup nel README.
9. Widget in Lua, 10. finestra delle impostazioni (in un processo suo).

Lasciati fuori: `aerospace` (niente tiling WM per decisione), `battery` (questo PC non ha una
batteria per provarlo), `notifications` (badge della taskbar e aggiornamenti alla brew non hanno
API leggere su Windows), di `now_playing` la scheda sotto il mouse, il titolo che scorre e la
riga della posizione (animazioni continue in DWM mentre suona), di `network` l'IP pubblico (per
decisione) e il Wi-Fi acceso e spento, di `audio` il collegamento dei dispositivi Bluetooth.

- `rustybar msg <comando>` (fatto): `ping` (`pong`), `reload` (rilegge la config anche se non è
  cambiata; risponde `ok` o l'errore), `query` (ogni widget con tipo, lato, `shown`/`hidden` e
  `popup open`; ogni schermo con dimensioni, scala, visibile o nascosto, e il rettangolo di ogni
  pill in pixel dello schermo; `game in front`), `popup <widget>` (apre il popup sotto la pill
  dello schermo col mouse, o del primo schermo che la mostra; se è aperto lo chiude come un clic
  sulla pill), `quit`. `trigger` arriva coi widget in Lua, gli unici che lo ascoltano. Trasporto:
  `WM_COPYDATA` alla finestra di controllo della cartella di config (la stessa di `rustybar
  quit`), risposta con `WM_COPYDATA` alla finestra message-only del processo `msg` (solo a una
  finestra di classe `RustyBarMsg`, qualunque handle arrivi), che aspetta 5 s; il processo `msg`
  chiama `AllowSetForegroundWindow` per la barra, così il popup prende il primo piano quando `msg`
  parte dall'app davanti (una scorciatoia, il terminale), e apre il filtro dei messaggi alla
  risposta (da un terminale elevato). Costo a riposo: nessuno, è un messaggio come gli altri.
- `rustybar.local.toml` (fatto): accanto a `rustybar.toml`, fuso sopra (tabelle chiave per
  chiave, il resto sostituito, come su macOS); il cambio di uno dei due ricarica la barra;
  `rustybar check` controlla l'unione e nomina tutti e due i file.
- `timezone` di `clock` (fatto): un fuso di Windows per nome di chiave (`Tokyo Standard Time`) o
  IANA (`Asia/Tokyo`, `UTC`, `Asia/Kolkata`), con la tabella IANA → Windows di CLDR nel binario
  (`widgets/zones.txt`, 614 nomi, 23 KB, scritta da ICU di Windows col test ignorato
  `print_zones`); il fuso si legge dalla sua chiave in `HKLM\...\Time Zones` (`TZI`, `Std`, `Dlt`)
  con il nome della chiave, così Windows applica anche le regole dei singoli anni
  (`SystemTimeToTzSpecificLocalTimeEx`). Misurato: ICU all'avvio costava 90 ms e l'elenco dei fusi
  di Windows (`EnumDynamicTimeZoneInformation`) 65 ms, sul primo frame; la tabella e la chiave
  non costano nulla. Per distinguere i due orologi un testo fra apici nella data: `date = "'Tokyo'"`.
  Calendario, «Oggi» e mezzanotte nel fuso dell'orologio.
- `power` (fatto, confermato con l'utente col mockup: secondo clic, nome dell'account, primo a
  sinistra nel layout predefinito): il logo di Windows nel colore del testo, come la mela su
  macOS (chiesto dall'utente l'11/11/2026 al posto del tasto d'accensione `E7E8`), da solo sul
  desktop senza sfondo né bordo (`Pill::bare`, chiesto dall'utente l'11/11/2026, come `apple` su
  macOS: la pill tiene la sua misura, quindi posizione e clic restano). Nessun font
  di sistema lo ha (Segoe Fluent Icons no, Wingdings solo la vecchia bandiera), quindi lo si
  disegna: `Item::windows_logo` (misurato come il glifo `E8A9` della stessa misura) e
  `ui::logo`, quattro quadrati grandi quanto `font_size`, uno spazio di 1/12 (almeno 1 px),
  allineati ai pixel a ogni scala; disegnati in `bar::draw_text` e nel popup ⋯. Nessuna
  immagine del marchio nella repo. Popup a menu (righe di 32 DIP,
  icona, testo, scorciatoia in `subtext` a destra; largo quanto serve, almeno 240): «Informazioni
  su questo PC» (`ms-settings:about`) | «Gestione attività» Ctrl+Maiusc+Esc (`taskmgr.exe`),
  «Impostazioni» Win+I (`ms-settings:`) | «Sospendi» (`SetSuspendState` su un thread dopo 0,3 s:
  Windows avvisa tutte le finestre e la chiamata torna al risveglio), «Riavvia il sistema»,
  «Arresta il sistema» | «Blocca» Win+L (`LockWorkStation`), «Disconnetti <nome>» | «Config di
  RustyBar…» (apre `rustybar.toml` con l'app di Windows o il Blocco note, scrivendo l'esempio se
  manca; la finestra delle impostazioni la sostituirà). Riavvia, Arresta e Disconnetti al
  secondo clic (la riga in rosso: «Fai clic di nuovo per arrestare»), con `ExitWindowsEx` come
  Start (le app possono dire che sono occupate): `EWX_REBOOT`, `EWX_SHUTDOWN | EWX_POWEROFF` più
  `EWX_HYBRID_SHUTDOWN` se `HiberbootEnabled` (avvio rapido), più `EWX_RESTARTAPPS` se
  `Winlogon\RestartApps`; `EWX_LOGOFF`. Prima `SeShutdownPrivilege` acceso nel token. Se Windows
  rifiuta, la riga dice «Non riuscito». Il nome: `NetUserGetInfo` livello 2 (`netapi32.dll`
  caricata solo all'apertura e lasciata subito; la barra non la carica), altrimenti il nome
  utente. `powrprof.dll` e `user32.dll` la barra le carica già. Costo a riposo: nessuno.

Decisioni (con l'utente, 11/11/2026, per `home`, `events`, `docker`): segreti (token di Home
Assistant, indirizzi iCal segreti) nel **Gestore credenziali** di Windows, scritti con `rustybar
secret set <nome>` da stdin; **`home`** con la connessione sempre aperta mentre una barra si vede
(WebSocket di WinHTTP, solo le entità delle tessere, keepalive di WinHTTP), chiusa senza barre
visibili, e la casa di Segoe al posto del logo di Home Assistant (marchio); **`events`** riscarica
i calendari ogni 15 minuti (più all'avvio, al risveglio e a mezzanotte), analisi su un thread di
lavoro, conti alla rovescia ogni minuto dai dati già letti; **`docker`** con Docker spento riprova
la named pipe ogni 30 s, fermo senza barre visibili e con un gioco della lista.

- Rete e segreti: `net.rs` (WinHTTP: `request` sincrona per un thread di lavoro, con proxy di
  sistema e gzip; `Socket` WebSocket asincrono: i callback di WinHTTP, sui suoi thread, mettono i
  messaggi in coda e impostano l'evento del widget; mai chiamate a WinHTTP col lock preso, perché
  possono richiamare subito sullo stesso thread; ogni handle con contesto tiene un `Arc` della
  connessione, lasciato a `HANDLE_CLOSING`; dopo l'upgrade nessun timeout di ricezione),
  `secret.rs` (credenziali generiche `rustybar-<nome>`, `CRED_PERSIST_LOCAL_MACHINE`; `rustybar
  secret set|delete|check <nome>`; provato con un nome usa e getta: le scritture dal processo
  dentro il pacchetto Claude finiscono nel Gestore vero), `mock.rs` (`RUSTYBAR_MOCK=<cartella>`:
  i widget coi dati personali leggono `<cartella>/<widget>.json`; `examples/mock`).
- `home` (fatto, sopra le decisioni): pill con la casa (`E80F`) nel blu di Home Assistant
  (`#18bcf2`) e quanti dispositivi delle tessere sono accesi (luci, prese, ventole,
  input_boolean accesi; clima non spento; robot che pulisce), grigia senza conteggio se il
  server non risponde; mentre la stampante stampa, davanti la stampante (`E749`) e la
  percentuale (gialla in pausa); compatta senza stampante. Popup (324 DIP) come su macOS: gruppi
  di `groups` col titolo, tessere di 150 DIP due per riga (alte come la più alta della coppia):
  icona (lampadina `EA80`, presa/interruttore `E7E8`, tapparella `E70E`/`E70D`, robot `E99A`,
  o `icon` della tessera come glifo o codice esadecimale: un nome di simbolo macOS è ignorato),
  nome, stato nel colore dello stato; luce = clic accende/spegne, cursore se regolabile
  (inviato al rilascio, 0 spegne); tapparella = clic e cursore della posizione; clima largo due
  tessere (termometro `E9CA` nel colore della modalità, nome, modalità, temperatura della stanza;
  acceso: le modalità come chip di testo su più righe e − obiettivo + inviato un secondo dopo
  l'ultimo clic); robot pulisce/torna; presa della stampante larga con la percentuale, la barra
  e «ancora 1 h 20 min», spenta solo al secondo clic con `printer.off`; script e scene come chip
  (in esecuzione la clessidra `E916`, fallito `E783`); non disponibile grigio senza azioni;
  servizio fallito «Non riuscito» in rosso finché l'entità cambia; «Apri Home Assistant»
  (`dashboard`). Valori impostati dal popup mostrati per 15 s o finché Home Assistant li ha.
  Fonti: `subscribe_entities` con gli id delle tessere e dei sensori della stampante (eventi
  compressi `a`/`c`/`r`), riconnessione dopo 1 s e poi ogni 10 s (grigia solo dal secondo
  fallimento), subito al risveglio; senza barre visibili il socket è chiuso. Servizi con `POST
  /api/services/<dominio>/<servizio>` su un thread di lavoro, il risultato torna con lo stesso
  evento. Un solo `wake::Timer` per la prima scadenza (nuovo tentativo, note, valori in sospeso,
  obiettivo del clima). Dati finti in `examples/mock/home.json` (quelli della versione macOS).
- `events` (fatto): come su macOS, dai calendari iCal (`calendars = [{ name, color, url? }]`,
  l'indirizzo segreto in `events-<nome>` del Gestore credenziali; mai l'indirizzo nel log). Pill:
  calendario (`E787`), ora d'inizio nel colore della distanza (giallo da `soon` 30, pesca da
  `sooner` 15, rosso da `imminent` 5, `primary` in corso), «Titolo · tra 12 min» o «· fino alle
  10:30 (ancora 20 min)»; senza eventi a orario quelli di tutto il giorno; compatta l'ora sola.
  Popup (324 DIP): riquadro dell'evento in evidenza (in corso quello che finisce prima finché
  non inizia il prossimo, altrimenti il prossimo) con orari, titolo, calendario · luogo, ospiti
  che hanno accettato (`ATTENDEE` con `PARTSTAT`), avanzamento in corso, «Partecipa a Google
  Meet» (`X-GOOGLE-CONFERENCE`, `URL`, descrizione o luogo); chip per quelli di tutto il
  giorno; la giornata a righe col colore del calendario, i passati attenuati e la linea rossa
  dell'ora; clic = la chiamata o il giorno in Google Calendar (`open`); «Apri Google Calendar».
  `ical.rs`: righe ripiegate, testo con escape, `DTSTART`/`DTEND`/`DURATION` con `TZID` (IANA o
  Windows, con `clock::time_zone` e `TzSpecificLocalTimeToSystemTimeEx`: le regole di ogni anno),
  UTC o fluttuanti, date intere; `RRULE` (`FREQ` giornaliera, settimanale, mensile, annuale;
  `INTERVAL`, `COUNT`, `UNTIL`, `BYDAY` con posizione, `BYMONTHDAY`, `BYMONTH`), `EXDATE`,
  `RECURRENCE-ID` (spostati o annullati), `STATUS:CANCELLED`; solo gli eventi del giorno, con
  gli orari in UTC. Download (`net::request`, gzip) e analisi su un thread di lavoro ogni 15
  minuti (`interval`), all'avvio, al risveglio, al cambio d'ora e a mezzanotte; non senza barre
  visibili né con un gioco della lista; un calendario che fallisce tiene i suoi eventi del giorno.
  Timer: allineato al minuto solo mentre c'è un evento a orario da contare, altrimenti ogni
  `interval`. Dati finti in `examples/mock/events.json` (minuti relativi ad adesso).
- `docker` (fatto): come su macOS, dall'Engine API sulla named pipe `\\.\pipe\docker_engine`
  (`DOCKER_HOST=npipe:////./pipe/<nome>` per un'altra): HTTP/1.1 scritto a mano con
  `Connection: close`, risposta letta fino alla chiusura della pipe (`parse_response`: lunghezza
  o chunked). Pill: scatola `E7B8` nel blu di Docker (`#1d63ed`) e quanti girano (anche in
  pausa), grigia senza, rossa se uno non è in salute, si riavvia o è morto; nascosta senza
  Docker. Popup (324 DIP): motore (`/info`: sistema, versione, CPU, memoria; a destra CPU e
  memoria di tutti); i container per progetto Compose (titolo, su/totali, ▶ ■ ↻ per tutti), poi
  «Container»; tessere di 150 DIP due per riga, al massimo 16 (il popup non scorre: «e altri N»):
  icona nel colore dello stato, CPU · MB, nome senza il prefisso del progetto, immagine, porte
  come link (`port:<n>` → `http://localhost:<n>`), da quanto è su (`/containers/<id>/json`,
  `StartedAt`) o lo stato («Uscito con codice 1»), ▶ (riprende se in pausa) ■ ↻ e cestino (al
  secondo clic, «Fai clic per eliminare»); in corso «Si ferma…», fallito «Non riuscito» (il
  motivo nel log). «Spazio su disco» (`/system/df`, all'apertura e dopo una pulizia): immagini,
  container fermi, volumi anonimi, cache di build con usato e liberabile; pulizia al secondo
  clic come `docker … prune` (volumi solo anonimi), «Reti inutilizzate», «Pulisci tutto» (come
  `docker system prune -a`), poi «Liberati 1,2 GB». «Apri Docker Desktop». Fonti: il flusso
  `/events` (solo container) su un thread suo con letture overlapped e un evento di stop
  (`Stream`, fermato e atteso al `Drop`): qualunque dato dopo l'intestazione = qualcosa è
  cambiato, rilettura un secondo dopo l'ultimo; letture, azioni, statistiche (`/stats` di
  ognuno in parallelo, ogni 5 s solo a popup aperto) e disco su thread di lavoro; un solo
  `wake::Timer` per la prima scadenza. Docker spento: pipe riprovata ogni 30 s; niente senza
  barre visibili né con un gioco della lista. Dati finti in `examples/mock/docker.json`.

## Domande aperte (da decidere con l'utente)

- Nessuna per ora.

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
  cambio di tema chiaro/scuro e di accento al volo, il cambio di DPI.
- 10/11/2026, **popup e clic**: clic sulle pill, popup (`popup.rs`, `ui::Scene`) e il
  calendario di `clock`, con le decisioni sopra. Provato sulla barra di prova del secondo schermo
  con messaggi alla finestra (il mouse non si muove; WoW era aperto): popup 260×292 px allineato
  al bordo destro della pill a 6 px sotto la barra; mese successivo, rotella, ←/→, Home ed Esc
  giusti (dicembre 2026 parte di martedì, ottobre di giovedì); la finestra sparisce a fine
  dissolvenza. Con un messaggio simulato `SetForegroundWindow` è rifiutato (il processo non ha
  ricevuto input vero): lo dice il log e il popup resta senza primo piano. Misure (barra di
  prova, due barre visibili): a popup aperto +0,3–0,7 MB privati e +0,3–0,6 MB committed. Senza
  pulizia, 251 aperture e chiusure: committed da 10,4 a 12,2 MB e poi fermo, handle costanti
  (nessuna perdita, sono cache e pool). Con la pulizia a popup sparito (`Gfx::trim`:
  `ID2D1Device::ClearResources` e `IDXGIDevice3::Trim`; `process::release_memory`:
  `HeapSetInformation(HeapOptimizeResources)`, come `release_memory` su macOS): 5,9 MB committed
  subito dopo la prima chiusura, 6,8 dopo 51, 8,2 un minuto dopo (ridisegnato l'orologio)
  contro 8,6 di una barra appena avviata. A riposo dopo i popup: 3,9 MB privati, 0 ms di CPU
  (16 ms al minuto con due barre: lo scatto del minuto), ~0,6 risvegli/s, GPU 0, 0 figli.
  Disegnando in fretta (50 popup di fila) WARP fa crescere il pool di thread di Windows fino a
  ~28; i worker inattivi escono da soli in circa un minuto (di nuovo 7–9). `measure.ps1` ora
  conta i risvegli thread per thread (prima un thread che usciva rendeva il totale negativo).
  Binario ~680 KB. Non provato con un mouse vero: il primo piano preso e restituito, la chiusura
  col clic altrove, la tinta dell'hover (con un movimento simulato Windows manda subito
  `WM_MOUSELEAVE`), l'animazione d'apertura a occhio, il clic su un giorno (aprirebbe il
  browser). Da fare: `system`, `audio`, `network`, `claude` (specifiche da confermare con
  l'utente); installazione e avvio con Windows.
- 10/11/2026, **`system`**: specifica confermata con l'utente (pill con la temperatura della GPU,
  5 s, popup completo, solo i motori 3D), WARP sul thread chiamante, pill misurate in cache,
  `Signal::Game`. Provato su una barra di prova sullo schermo sotto con WoW in primo piano
  (quella dello schermo principale nascosta): il popup aperto con messaggi alla finestra non ha
  preso il primo piano. Costi misurati: una lettura a riposo ~0,7 ms (di cui ~0,3 ms nel driver
  per il motore 3D) più ~0,7 ms per misurare e ridisegnare la pill; a popup aperto ogni 2 s
  ~6 ms di lettura (5 ms la chiamata dei processi) + 0,4 ms di scena + 2,4 ms di disegno
  (344×836 px con quattro dischi). A riposo per 60 s, dopo 21 popup: 4,7 MB privati (8,4 MB
  committed), 16 ms di CPU, 1,8 risvegli/s, 2 thread, GPU 0, 0 figli; prima di togliere il pool
  di WARP erano 11 thread e 6,3 risvegli/s. Popup aperto: +1,9 MB privati; chiuso: di nuovo
  3,7–3,8 MB, stabile dopo 20 aperture. Primo frame 32 ms (GPU aperta in ~7 ms all'avvio).
  Binario ~740 KB. Non provati: il tema chiaro, la GPU assente (`gpu = false` la toglie), il
  clic su un disco e sui pulsanti (aprirebbero finestre).
- 11/11/2026, **`audio` e `media`**: specifiche confermate con l'utente (sopra). Misurati con un
  probe prima del codice: Core Audio +0,3 MB privati e 11 ms all'avvio, GSMTC +0,8 MB e 13 ms
  (asincrono, fuori dal primo frame). Barra di prova sullo schermo sotto con WoW in primo piano e
  un video in riproduzione in un browser: pill giuste (cuffie USB al 100%, il video con la nota),
  popup aperti con messaggi alla finestra senza primo piano: volumi coi cursori, 6 uscite e
  2 ingressi col predefinito acceso, player con copertina, app e posizione che avanza. A riposo
  in un minuto i soli risvegli del thread principale sono le 12 letture di `system` (tracciato
  nel log): `audio` e `media` niente. Misure a riposo dopo i popup: 5,0 MB privati (8,2 MB
  committed), 0–31 ms di CPU al minuto, 2,5 risvegli/s, 5 thread, GPU 0, 0 figli; popup di
  `media` aperto: 16 ms di CPU in 20 s (il ridisegno della posizione ogni secondo), di `audio`
  0 ms. Il popup di `media` lasciava +2,4 MB: la cartella App di Start per il nome (1,2 MB, 54 ms)
  e il compilatore di shader di Direct2D per la copertina interpolata cubica (`d3dcompiler_47`);
  tolti entrambi, dopo il popup la barra torna come appena avviata (5,0 MB). Primo frame 54 ms
  (Core Audio all'avvio). Binario ~820 KB. Non provati, perché cambierebbero lo stato del PC:
  il trascinamento del volume, il muto, il cambio di dispositivo predefinito, play/pausa e la
  ricerca nel brano; la batteria Bluetooth (nessuna uscita Bluetooth attiva: il percorso
  endpoint → contenitore → nodi Bluetooth è provato, cinque nodi Bluetooth hanno la batteria);
  lo sgancio dei callback senza barre visibili.
- 11/11/2026, **`network`**: specifica confermata con l'utente (sopra). Qui: Ethernet a 1 Gb/s
  con Internet, una seconda scheda Ethernet scollegata, nessun Wi-Fi (servizio WLAN spento).
  Barra di prova sullo schermo sotto: pill «Ethernet», popup con connessione, download e upload
  che si riempiono, dettagli e ping al router (sotto il millisecondo: «< 1 ms»). A riposo per
  60 s: 5,6 MB privati (8,7 MB committed), 16 ms di CPU, 2,6 risvegli/s, 7 thread, GPU 0,
  0 figli; popup aperto: 31 ms di CPU in 20 s, +0,5 MB. Binario ~890 KB. Non provati: tutto il
  Wi-Fi (nessuna scheda: pill, elenco, scansione, collegamento, permesso di posizione), la VPN,
  la copia negli appunti (cambierebbe gli appunti), «Nessun Internet» e «Disconnesso» (servirebbe
  staccare la rete).
- 11/11/2026, **`claude`**: specifica confermata con l'utente (sopra). Lo schermo sotto si è
  spento durante le prove: con il permesso dell'utente la barra di prova è andata sullo schermo
  principale. Senza account: pill `✻ Accedi`, popup «Nessun account» con la cartella e
  «Accedi»; l'utente ha completato l'accesso nel browser e la pill è passata a
  `✻ 29% · 46% · 15:10`, il popup a nome, piano, età, sessione e settimana. Costi misurati di
  una lettura con l'account collegato: `claude -p` 0,41–0,49 s, ~0,4 s di CPU, ~186 MB di picco;
  `auth status` 0,29 s, 0,25 s di CPU, 152 MB; il figlio parte a priorità 4 (idle). Ogni 5 minuti
  fanno ~0,13% di un core in media, nei sei decimi di secondo in cui gira. La barra a riposo fra
  una lettura e l'altra: 5,2 MB privati (8,3 MB committed), 0 ms di CPU in 60 s, 2,05 risvegli/s,
  0 figli. Primo frame 46–59 ms. Binario ~1,1 MB (processi, thread, JSON). Fuori dal layout
  predefinito perché costa: si aggiunge in `[layout]`. Non provati: «Esci» (toglierebbe il
  login), il timeout di un `claude.exe` appeso, l'accesso fallito, un secondo account.
- 11/11/2026, **installazione e avvio con Windows** (scelte dell'utente: chiave Run, voce in
  Impostazioni > App, config di esempio se manca, collegamento nel menu Start; installata e
  avviata su questo PC). `install.rs`: `rustybar install` copia l'eseguibile in
  `%LOCALAPPDATA%\Programs\RustyBar` (prima chiude la barra della cartella predefinita,
  `process::quit` con attesa, e ritenta la copia per 5 s: lo stesso comando aggiorna), scrive
  `config/rustybar.toml` (dentro l'eseguibile con `include_str!`) in `%APPDATA%\rustybar` se non
  c'è, `HKCU\...\Run\RustyBar`, `HKCU\...\Uninstall\RustyBar` (nome, versione, editore,
  dimensione, `UninstallString` = `rustybar.exe uninstall`, `NoModify`/`NoRepair`), il
  collegamento `RustyBar.lnk` in `FOLDERID_Programs` (`IShellLinkW`), e avvia la barra staccata
  dal terminale (`DETACHED_PROCESS`, fuori dal job del terminale se lo permette, senza
  `RUSTYBAR_CONFIG_DIR`). `rustybar uninstall` toglie tutto e lascia config e log; lanciato
  dalla copia installata (come fa «Disinstalla» di Impostazioni) sposta l'eseguibile in uso nei
  file temporanei (`rustybar-uninstalled-<pid>.exe`, si può spostare ma non cancellare), che il
  prossimo install o uninstall cancella. I messaggi non usano `println!`: su una pipe chiusa va
  in panico e con `panic = "abort"` l'installazione restava a metà (successo davvero, con
  `| head`). Provati qui: installazione (barra visibile, area di lavoro da y = 40, primo frame
  53 ms, 4,9 MB privati), disinstallazione dalla copia installata (tutto tolto, config rimasta,
  copia parcheggiata), reinstallazione (config lasciata com'era, parcheggiata cancellata),
  aggiornamento con la barra accesa anche con l'output chiuso. Non provati: l'avvio vero
  all'accesso a Windows, «Disinstalla» cliccato in Impostazioni (esegue lo stesso comando).
- 11/11/2026, **install fuori dal pacchetto**: dopo il riavvio della sessione la barra installata
  non girava più (morta col job della sessione di prima, da cui era stata avviata), e guardando
  da fuori la chiave Run, la voce in Impostazioni e la config non esistevano: erano finite nel
  registro e nella cartella privati del pacchetto Claude (vedi «Ambiente e comandi»), quindi
  all'accesso la barra non sarebbe partita. Ora `install`/`uninstall` (`install::run`) scrivono un
  collegamento temporaneo in `%TEMP%` a se stessi con `<comando> --result <file>`, lo fanno aprire
  a `explorer.exe` e stampano le righe che quel processo scrive nel file (fino a `rustybar-exit:N`);
  se Esplora risorse non lo avvia entro 10 s fanno da sé, avvisando se il processo è in un
  pacchetto. La barra la avvia `explorer.exe <exe>` (figlia della shell, come all'accesso, con
  l'ambiente della shell), con l'avvio diretto come ripiego. Verificato da un processo esterno:
  chiave Run, voce in Impostazioni e config nel registro e nella cartella veri; barra figlia di
  `explorer.exe`; le copie private tolte; la config dell'utente rimessa nel file vero. Tre account
  `claude` (`claude_2` e `claude_3` con `dir` e `color` loro, collegati dall'utente con «Accedi»)
  mostrano ognuno i suoi numeri nel suo colore.
- 11/11/2026, **le altre implementazioni**: confrontata la versione macOS e scelto con l'utente
  cosa portare (vedi «Dalla versione macOS»); fix dell'install committato e pushato da solo
  (`e6d5abb`). L'utente ha dato il permesso per tutto questo lavoro di provare con una barra di
  prova sullo schermo principale (quello sotto è spento), sopra quella vera, con popup aperti da
  messaggi e screenshot della striscia cancellati dopo.
- 11/11/2026, **`msg`, `rustybar.local.toml`, `timezone`**: barra di prova con un
  `rustybar.local.toml` che aggiunge un orologio di Tokyo: unione giusta (`check` nomina i due
  file), Tokyo 8 ore avanti, `msg ping`/`query`/`popup`/`reload`/`quit` giusti, il popup aperto da
  `msg popup` sotto la pill dello schermo col mouse (senza primo piano, come i messaggi
  simulati), errori per un widget senza popup o sconosciuto. Primo frame con l'orologio di Tokyo:
  55–63 ms (con ICU e l'elenco dei fusi di Windows era 207–218 ms; senza orologio 56–58 ms).
  Nessun costo a riposo: niente da ascoltare, niente timer. Non provati: `msg` da un terminale
  elevato, il popup che prende il primo piano da una scorciatoia vera.
- 11/11/2026, **`power`**: barra di prova sullo schermo principale: pill di 34×28 px all'inizio
  a sinistra, popup aperto con `msg popup power` (righe, scorciatoie e nome giusti); un solo
  clic simulato su «Arresta il sistema» la fa diventare rossa con «Fai clic di nuovo per
  arrestare», poi chiuso con `msg popup` (che azzera la conferma). A riposo per 60 s con `power`
  e due orologi: 4,6 MB privati (7,8 MB committed), 0 ms di CPU, 3,4 risvegli/s, 4 thread, GPU 0,
  0 figli; primo frame 58 ms. Non provati, perché spegnerebbero o bloccherebbero il PC: il secondo
  clic su Riavvia/Arresta/Disconnetti, Sospendi, Blocca, e i link (aprirebbero finestre).
- 11/11/2026, **extra del popup di `media`** (scelta fra le app, shuffle e ripetizione, volume
  dell'app, colori dalla copertina; ripreso in una sessione nuova dopo il limite di 5 ore). Provati
  con due copie del player finto (`Aurora.exe` e `Nimbus.exe`, brani inventati) sulla barra di
  prova dello schermo principale, coi popup aperti da `msg popup` e i clic da messaggi alla
  finestra: selettore con le due app, clic su Aurora → pill e popup su Aurora coi colori della
  sua copertina (blu e oro), shuffle acceso, ripetizione su un brano, volume dell'app al 50%
  (nel mixer); chiuso e riaperto il popup torna sulla sessione corrente. Costi: volume dell'app
  trovato in 5–6 ms all'apertura (6 uscite attive), colori in ~3 ms sul thread del pool. A
  riposo con due sessioni: 5,6 MB privati, 16 ms di CPU in 30 s; popup aperto: 6,4 MB, 31 ms di
  CPU in 20 s (il ridisegno della posizione ogni secondo); chiuso: 5,7 MB, 0 ms in 30 s. Non
  provati: app vere (Spotify, browser), un'app pacchettizzata (volume per AUMID), il tema chiaro
  coi colori della copertina a occhio (il test controlla i contrasti).
- 11/11/2026, **la barra che non ci sta** (forme compatte e ⋯, sopra). Provata sulla barra di
  prova dello schermo principale stringendo lo spazio con `[bar] margin` (2000–2410 DIP per lato
  su 5120): prima `media` al centro compatto (275 → 149 px), poi rete, audio e `system` compatti,
  poi l'orologio con la sola ora e rete, audio e `system` nel ⋯ di destra, `front_app` in quello
  di sinistra; popup ⋯ con le pill piene allineate a destra; clic su «Ethernet» lì dentro → popup
  di `network` sotto il ⋯; tolto il margine torna tutto pieno. A riposo dopo le prove, per 60 s:
  6,4 MB privati, 0 ms di CPU, 2,5 risvegli/s, 7 thread. Binario ~1,4 MB. Non provati: uno schermo
  stretto vero (o al 150%), `workspaces` cliccato dentro il ⋯ (cambierebbe desktop), il tema
  chiaro del popup ⋯.
- 11/11/2026, **`home`** (con `net.rs`, `secret.rs`, `mock.rs`; decisioni sopra). Provato coi dati
  finti (`RUSTYBAR_MOCK=examples\mock`) sulla barra di prova: pill «stampante 45% · casa 4»,
  popup con tessere, clima con le modalità su due righe, stampante con la barra, robot, chip e
  «Apri Home Assistant». `rustybar secret set/check/delete` provato con un nome usa e getta. Non
  provati: un Home Assistant vero (serve il token dell'utente: la connessione WebSocket, gli
  eventi, i servizi, la riconnessione), il tema chiaro.
- 11/11/2026, **`events`** (decisioni sopra; specifica in «Dalla versione macOS»). `ical.rs`
  provato coi test (righe spezzate, parametri tra virgolette, `RRULE` settimanali, mensili col
  n-esimo giorno e l'ultimo, annuali con `UNTIL`, `EXDATE`, `RECURRENCE-ID` spostato, annullato,
  tutto il giorno, Roma d'inverno e d'estate) e con il calendario pubblico delle festività italiane
  di Google (test ignorato `a_calendar_of_google`: scaricato e letto in 0,23 s, Natale c'è). Dati
  finti sulla barra di prova: pill «13:42 Design review · tra 12 min» in pesca, popup come su
  macOS. A riposo per 60 s col calendario pubblico: 6,1 MB privati, 31 ms di CPU, 2,25
  risvegli/s, 8 thread. Non provati: un calendario segreto vero dell'utente, un evento in corso
  vero, la mezzanotte.
- 11/11/2026, **`docker`** (decisioni sopra). Qui Docker Desktop non c'è: provato coi dati finti
  sulla barra di prova (pill «scatola 4» in blu; popup con motore, progetto `shop` 3/4, tessere
  verdi, gialla per il controllo di salute che parte, rossa per l'uscita con codice 1, porte,
  disco con lo spazio liberabile) e coi test di `parse_response` (lunghezza, chunked),
  `container` (salute dallo `Status`, codice d'uscita, porte), `rfc3339`. Non provati: un Docker
  vero (pipe, flusso degli eventi, azioni, statistiche, pulizie), il riavvio di Docker.
- 11/11/2026, misura dopo `home`, `events`, `docker`: barra di prova senza dati finti con tutti e
  tre nel layout (Docker assente, riprovato ogni 30 s; `home` senza server; `events` col
  calendario pubblico), a riposo per 60 s: 6,0 MB privati (9,2 MB committed), 16 ms di CPU,
  2,9 risvegli/s, 8 thread, GPU 0, 0 figli; primo frame 51 ms. Binario ~1,85 MB (WinHTTP e i tre
  widget: ~0,4 MB). Prossimi: dati finti degli altri widget (rete, audio, `system`, `claude`,
  `power`, `front_app`, `workspaces`, `media`) e screenshot dei popup disegnati fuori schermo
  (mai una cattura dello schermo dell'utente), poi Lua e la finestra delle impostazioni (da
  decidere con l'utente).
- 11/11/2026, **logo di Windows in `power`** (chiesto dall'utente): quattro quadrati disegnati
  (`ui::logo`), visti ingranditi sulla barra di prova (6 px con 1 px di spazio al 100%, netti e
  centrati); installata la barra nuova e aggiunto `power` in testa a sinistra nella config vera
  dell'utente (ricaricata da sola, `msg query` lo mostra). Non visto: le scale 125–200% a occhio
  (il test controlla i pixel).
- 11/11/2026, **`power` senza sfondo** (chiesto dall'utente, in una sessione nuova): `Pill::bare`,
  nella firma dello sfondo; la pill `bare` non ha la superficie dello sfondo (una in meno da
  tenere). Barra di prova sullo schermo principale (`reserve = false`, `margin = 1200`): logo
  bianco netto sul desktop, nessuno sfondo né bordo, pill di 34×28 px nello stesso posto, accanto
  a `clock` con la sua pill. Popup ⋯ (`margin = 2459` e un orologio al centro, così `power` e
  `clock` finiscono nel ⋯): il logo da solo e l'orologio con sfondo e bordo. Nota per le prove
  del ⋯: col margine il popup resta a `margin` dai bordi dello schermo, quindi con un margine
  enorme non c'è posto e non si apre (`msg popup` dice comunque `ok`). Non visti: il tema chiaro
  (il logo nel colore del testo scuro sul desktop) e una scala diversa dal 100%.
