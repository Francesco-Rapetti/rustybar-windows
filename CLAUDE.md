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
  Per i popup (e per il README) meglio `rustybar msg shot <widget> <file.png> [scala]`: la barra
  li disegna fuori schermo, niente dello schermo dell'utente nella foto. La barra finta:
  `RUSTYBAR_MOCK`, `RUSTYBAR_CONFIG_DIR` = `examples\mock` e `RUSTYBAR_LOCALE=en-US` (sezione
  «Made-up data» del README); log `rustybar-mock.log`.
- La finestra delle impostazioni: le pagine si guardano con `rustybar settings --shot <pagina>
  <file.png> [scala] [larghezza]` (fuori schermo, come `msg shot`; con `RUSTYBAR_MOCK` la config
  finta di `examples\mock\settings`). Per provarla dal vivo senza disturbare: la cartella di una
  barra di prova (`RUSTYBAR_CONFIG_DIR`) e `RUSTYBAR_SETTINGS_TEST=1`: la finestra va fuori da ogni
  schermo (x = −20480) e non chiede mai il primo piano; la si guida con `PostMessage` alla classe
  `RustyBarSettings` (coordinate client; al 100% e con la finestra di 1040 DIP il contenuto va da
  x = 320 a 984) e si scrive nell'EDIT (classe `RustyBarSettingsEdit`, figlio `Edit`) con
  `WM_SETTEXT` + `WM_KEYDOWN` Invio. In PowerShell `FindWindow(classe, $null)` passa `""`: serve
  `[NullString]::Value`. Log `rustybar-settings[-<cartella>].log`. Mai provare dal vivo
  l'interruttore «Riserva lo spazio» su una barra di prova (la fa riservare) né un campo segreto
  fuori dai dati finti (scriverebbe nel Gestore credenziali vero). Per vedere cosa mostra la
  finestra di prova fuori schermo: `PrintWindow` con `PW_RENDERFULLCONTENT` (2) sulla sua finestra
  (Bitmap di `System.Drawing`), che fotografa lei sola, contenuto DirectComposition compreso; un
  `--shot` non basta, passa da un'altra strada (`Gfx::png`). La tinta dell'hover non si vede con
  movimenti simulati (Windows manda subito `WM_MOUSELEAVE`).
- In questa sessione `LOCALAPPDATA` è `%USERPROFILE%\.claude-account2-localappdata`: i log delle
  barre di prova sono lì, non nel `%LOCALAPPDATA%` vero.

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
  primo piano e tutte le barre visibili. Config: al cambio della cartella un `wake::Timer` di
  150 ms (`SETTLE_MS`) aspetta che le scritture finiscano, poi il timbro dice se è cambiato un
  file (un file letto a metà, troncato da chi scrive, dava per un attimo il layout predefinito).
  Ricarica fine (`reload`, `make_widgets`): ogni `Slot` ricorda tipo e opzioni; un widget con nome,
  tipo e opzioni uguali resta com'è (niente letture, connessioni o processi nuovi), gli altri
  vanno prima che arrivino i nuovi; lo stato Lua resta se nessun `.lua` è cambiato (il timbro dei
  `.lua`), con un `.lua` cambiato si rifanno i soli widget Lua; tema e font si rifanno solo se
  `[theme]` è cambiato, le barre solo se `[bar]`. I widget nuovi sentono `Foreground`, `Shown` e
  `Game`; quelli tenuti solo `Game` se è cambiato. `rustybar msg reload` rifà tutto. Il log dice
  «config reloaded: N of M widgets made again (nomi)».
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
  sfondo né bordo, stessa misura e stessa area del clic, anche nel popup ⋯; `Pill::part` = una
  parte della pill da un item in poi, il cui sfondo si riempie da sinistra fino a un livello in
  un colore (il volume di `audio`), tagliata alla forma della pill dentro il bordo, con una linea
  nel colore del bordo prima delle parti dopo la prima: `ui::parts` dà dove, disegnata col testo
  in `bar::paint_text` (`PushAxisAlignedClip` sul rettangolo arrotondato) e nel popup ⋯ con
  `Shape::Cut`, nella firma del testo; `Scene` e `Fonts` dei
  popup, `Backdrop` per i colori propri di un popup), `palette.rs` (i colori di un'immagine in
  Oklab, per il popup di `media`), `theme.rs` (le 24 palette copiate dalla versione macOS + `windows` dal registro:
  `AppsUseLightTheme`, `Accent\AccentPalette`; `level` per giallo e rosso, `tile` per i riquadri
  dei popup), `config.rs` (serde + toml, `deny_unknown_fields`, `rustybar check`,
  `rustybar.local.toml` fuso sopra con `merge`), `process.rs`
  (DPI per schermo, priorità sotto il normale, EcoQoS, memoria a priorità bassa, un'istanza per
  cartella; `make_small` anche per il figlio di `claude`; `quit` di una cartella con attesa;
  `run`: un figlio piccolo, senza finestra, priorità idle, EcoQoS, nel job `KILL_ON_JOB_CLOSE`,
  pipe lette mentre gira, timeout; lo usano `claude` e `rb.exec`),
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
  `network` le icone, `audio` le icone coi riempimenti, `system` la RAM, `claude` la
  sessione.
- COM: il thread principale è un apartment STA (`CoInitializeEx` in `app::run`, il ciclo ne
  pompa i messaggi). I callback di Core Audio e i gestori WinRT arrivano su thread loro: toccano
  solo atomici, mutex ed eventi (`wake.rs`: `Wake`, un evento auto-reset condiviso con `Arc`;
  `Timer`, un timer waitable che gira solo quando serve). Mai `join()` di un'operazione WinRT sul
  thread principale: si blocca (provato); si usa `when`. `windows-core` è una dipendenza
  diretta solo perché le macro `#[implement]` e `#[interface]` lo nominano.
- `widgets/`: un file per widget, registrato in `widgets::create` e in `KINDS`. Il trait `Widget`
  ha `pill`, `compact`, `wide` (la forma larga del gruppo centrale), `handle` (un handle da
  aspettare) o `handles` (più d'uno: un evento e un timer) con `signaled`/`signaled_at`
  (meglio riconoscere l'handle che contare gli indici: l'elenco cambia con ciò che gira), ed
  `event` (`Signal`: `Foreground`, `Resume`, `TimeChanged`, `Shown`, `Game` = un gioco della
  lista in primo piano, `DevicesChanged` = `DBT_DEVNODES_CHANGED`, `Displays` = schermi cambiati,
  `Trigger`); dicono se ciò che il widget mostra è cambiato (la pill, o il popup aperto). Nella
  pill `Item::icon` (Segoe Fluent Icons) e `Item::image` (una figura quadrata: la copertina); nei
  popup `Shape::Image`, `Scene::slider`, `Input::Slide`/`Release`, testi tagliati coi puntini.
  Moduli d'aiuto in `widgets/`: `mixer.rs` (il mixer di `audio`), `privacy.rs` (microfono e
  fotocamera in uso), `hid.rs` (le batterie dietro ricevitori e cavi, per `devices`). Fuori:
  `crash.rs` (il motivo di un crash nel log e la ripartenza).

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
  batteria Bluetooth anche nella pill): pill in due parti, come su macOS (chiesto dall'utente il
  14/11/2026: i nomi delle sorgenti e il volume come riempimento della pill; scelta da una bozza
  la disposizione «B», una parte per uscita e ingresso, con la tinta): ogni parte con l'icona, il
  nome del dispositivo (tagliato a 160 DIP) e il volume (`fixed("100%")`, grigio se muto), e lo
  sfondo riempito fino al volume nel `primary` al 30% (il colore del testo al 10% se muto;
  `Pill::part`); fra le due la linea del bordo e il `pill_padding` per lato. Uscita: l'icona per
  tipo (cuffie, cuffie con microfono, schermo HDMI/DP, altoparlanti per livello; barrata se
  muta); con un'uscita Bluetooth la batteria (icona a decimi di Segoe Fluent Icons, gialla al
  20%, rossa al 10%) dopo il volume. Ingresso: il microfono, rosso barrato se muto, `primary` se
  un'app lo usa, poi la fotocamera se un'app la usa. Un dispositivo solo (stesso contenitore,
  non quello del PC `{…FFFF-FFFFFFFFFFFF}` dove stanno altoparlanti e microfono della scheda
  audio, o lo stesso nome) ha il nome una volta. Il nome (`short_name`): Windows chiama gli
  endpoint `Cuffie (Aurora Buds)`; la parte fra parentesi, salvo quando è la scheda o il driver
  (una parola `Audio`: `Altoparlanti (High Definition Audio Device)` → `Altoparlanti`), senza i
  numeri di Windows (`2- `, `1 - ` dei monitor) né `Stereo`/`Hands-Free…` del Bluetooth; o quello
  dell'utente, `names` (per nome completo o della pill, dalla pagina del widget). `input =
  false`: la sola uscita, col microfono e la fotocamera come prima. Compatta: le icone coi
  riempimenti. Popup (320 DIP): un riquadro per l'uscita e
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
8. Dati finti (`RUSTYBAR_MOCK`) e screenshot dei popup nel README (fatto, sotto).
9. Widget in Lua (fatto, sotto), 10. finestra delle impostazioni (in un processo suo; fatta, sotto).

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
  sulla pill), `quit`, `trigger <nome> [chiave=valore…]` (per i widget in Lua, gli unici che lo
  ascoltano), `shot`. Trasporto:
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
  «Arresta il sistema» | «Blocca» Win+L (`LockWorkStation`), «Disconnetti <nome>» |
  «Impostazioni di RustyBar…» (lancia `rustybar settings`, un processo suo fuori dal job dei
  figli, dopo `AllowSetForegroundWindow`; prima era «Config di RustyBar…», che apriva il file, ora
  nella pagina Informazioni). Riavvia, Arresta e Disconnetti al
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
  sistema e gzip (solo gzip: con `deflate` chiesto, Open-Meteo lo manda e WinHTTP fallisce alla
  prima lettura con `E_ABORT`); `Socket` WebSocket asincrono: i callback di WinHTTP, sui suoi thread, mettono i
  messaggi in coda e impostano l'evento del widget; mai chiamate a WinHTTP col lock preso, perché
  possono richiamare subito sullo stesso thread; ogni handle con contesto tiene un `Arc` della
  connessione, lasciato a `HANDLE_CLOSING`; dopo l'upgrade nessun timeout di ricezione),
  `secret.rs` (credenziali generiche `rustybar-<nome>`, `CRED_PERSIST_LOCAL_MACHINE`; `rustybar
  secret set|delete|check <nome>`; provato con un nome usa e getta: le scritture dal processo
  dentro il pacchetto Claude finiscono nel Gestore vero), `mock.rs` (`RUSTYBAR_MOCK=<cartella>`:
  tutti i widget tranne `clock` leggono `<cartella>/<widget>.json`; `examples/mock`).
- Dati finti e screenshot (fatto, 11/11/2026): ogni widget ha il suo `examples/mock/<nome>.json`
  e con `RUSTYBAR_MOCK` non legge, non ascolta e non lancia niente del PC (niente Core Audio,
  GSMTC, IP Helper, WLAN, registro, processi, `claude.exe`, timer); ciò che cambierebbe il PC non
  fa nulla (comandi di `power`, cambio di dispositivo, appunti, collegamento al Wi-Fi, link),
  cursori, muto, selettori e player cambiano solo il dato finto, il clic su un desktop di
  `workspaces` sposta l'evidenziazione senza tasti. `power` il `user`; `front_app` il `name`;
  `workspaces` `desktops` e `current`; `claude` gli `accounts` per `dir` (`~/.claude` senza),
  `at` minuti fa e `reset` fra quanti minuti, la cartella mostrata com'è scritta nella config;
  `system` CPU, memoria e dischi in GB, GPU (`Gpu::mock`, senza scheda), storico, app; `audio`
  uscite e ingressi (`form`, `bluetooth`, `battery`, `default`) e volumi; `network` scheda,
  Wi-Fi, VPN, traffico, ping e reti (banda in GHz); `media` i player (`app` come AUMID, stato,
  durata e posizione in secondi, shuffle, ripetizione, volume, copertina da file accanto, decodificata
  con lo stesso WIC: `decode_cover_file`) e `current`. Copertine di `examples/mock/media` dalla
  versione macOS (disegnate, inventate). `examples/mock/rustybar.toml`: la barra degli screenshot
  con tutti i widget, `reserve = false` e l'accento di un Windows nuovo (`#4cc2ff`).
  `rustybar msg shot <widget|⋯lato|bar[:left,center,right]> <file.png> [scala]`: la barra disegna
  fuori schermo (`Gfx::png`: device context Direct2D sul device WARP, bitmap bersaglio, copia in
  una bitmap leggibile dalla CPU, PNG con WIC; `popup::shot`/`paint`, `bar::paint_pill`) il popup
  come appena aperto (`popup_shown(true)`, scena, `popup_shown(false)`; se è già aperto la sua
  scena) o le pill piene una dopo l'altra su `crust`; il processo `msg` rende assoluto il
  percorso. `RUSTYBAR_LOCALE` (`en-US`): lingua dei testi e formati di date e numeri
  (`i18n::locale()` al posto del null nelle funzioni `*Ex`), per gli screenshot in inglese.
  Costo a riposo: nessuno (una variabile d'ambiente letta all'avvio).
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

Decisioni (con l'utente, 12/11/2026, per i widget in Lua; specifica proposta con i costi misurati
da una sonda e confermata così com'era):

- **Dipendenza**: `mlua` 0.12 con Lua 5.4 compilato dentro (`lua54`, `vendored`), lo stesso della
  versione macOS; sempre nel binario (+~0,6 MB su disco: la sonda +584 KB), nessuna feature di
  cargo per toglierlo. Lo stato Lua nasce solo se ci sono `widgets\*.lua`: senza, costo 0 a
  riposo. Misurato: stato +24–28 KB privati, un widget +32–36 KB, `Lua::new` 0,2 ms, caricare
  un widget 0,1 ms, `update` + `bar` 1,3 µs, un popup di 8 righe 11 µs.
- **File**: `<cartella di config>\widgets\*.lua`, i nomi in `[layout]`, `[widgets.<nome>]` come
  `self.options`.
- **API come su macOS dove si può**: `rb.widget { name, init, every, update, bar, compact, popup,
  event, on }`; pill di `text` e `icon` (codici di Segoe Fluent Icons) con colori della palette,
  `small`, `fixed`, `max_width`, `highlight`, `bare`, `on_click`; `rb.redraw`, `rb.open`,
  `rb.close_popup`, `rb.trigger`, `rb.color`, `rb.theme`, `rb.t(it, en)`, `rb.http` (WinHTTP su un
  thread di lavoro, nessun processo) e `rb.exec` (un processo nascosto, priorità idle, nel job che
  muore con la barra, timeout 30 s: costa un processo, documentato). `rb.home` dopo.
- **Popup (b)**: una colonna dall'alto in basso coi pezzi nello stile dei popup di serie (testo,
  sezione, riga etichetta/valore, pulsante, cursore, barra, riquadro, riquadri due per riga,
  divisore, spazio, immagine), non il motore di layout completo della versione macOS.
- **Eventi**: `on = { "wake", "foreground", "time", "game", "shown", "<trigger>" }` →
  `event(self, nome, args)`; `rustybar msg trigger` e `rb.trigger` ai widget Lua.
- **Ricarica ed errori**: un `.lua` cambiato ricarica tutti i widget Lua; `rustybar check` carica
  gli script; un widget che sbaglia mostra `⚠ nome` in rosso e l'errore nel log e nel popup.
- **Limiti**: 250 ms per chiamata, niente `os.execute`/`io.popen`/`os.exit`, `every` almeno 1 s,
  timer fermi senza barre visibili e con un gioco della lista, 16 MB al massimo, GC completo alla
  chiusura di un popup, nessun widget Lua nel layout predefinito.
- Aggiunti scrivendo gli esempi (detti all'utente): `self.every` (`false` ferma il timer di uno
  script, un numero lo riparte: un pomodoro fermo non sveglia la barra ogni secondo) e
  `rb.json(testo)` (Lua non ha JSON; il lettore di `json.rs`).

- Widget in Lua (fatto, 12/11/2026, come le decisioni sopra): `lua.rs` è lo stato (`Host`, in un
  `Rc`, clonato da ogni widget Lua): `Lua::new()` (le librerie sicure) con `os.execute`,
  `os.exit`, `io.popen`, `package.loadlib` tolti e `package.cpath` vuoto, `package.path` =
  `widgets\lib\?.lua`; `set_memory_limit` a 16 MB; un hook ogni 10 000 istruzioni guarda la
  scadenza (`BUDGET`, 250 ms) messa da `Host::call` (una chiamata dentro un'altra tiene quella di
  fuori); `rb` (globale e `require("rustybar")`) con `widget`, `redraw` (vuota), `open`,
  `close_popup` (un flag che `popup_input` legge: `Reply::Close`), `trigger` (`Event::Trigger`
  nella coda dell'app → `Signal::Trigger` a tutti), `color`, `theme` (rifatto da `set_theme` a
  ogni cambio di tema), `t`, `json`, `log` (anche `print`), `http` (`net::request` su un thread di
  lavoro) ed `exec` (`cmd.exe /D /S /C` con `process::run`, 30 s); il preludio Lua fa `rb.ui`
  (ogni costruttore mette `_t`). Il lavoro in sottofondo torna per `Channel`: ogni widget ha il
  suo evento (`Wake`) e i suoi callback per id (al massimo 8 insieme); `Host::call` dice di chi
  è la chiamata in corso. `widgets/lua.rs`: `LuaWidget` (`self` = tabella sua con `name` e
  `options` dal TOML, `__index` alla definizione; `init` e `update` alla creazione; `every`
  riletto dopo ogni chiamata che può cambiarlo; slot: l'evento e il timer), pill da `bar`
  (`Item`/`Pill`; `fixed` vuole `&'static str`: i modelli tenuti per sempre, al massimo 256),
  compatta da `compact` o `bar` con `ctx.compact`, azioni delle pill (funzione, URL, `"popup"`;
  quelle degli elementi valgono per la forma piena), popup (`Lay`: colonna con `gap` 4, rientro
  di 10 DIP per le righe, riquadri disposti prima a parte, `Scene::append`, per sapere l'altezza;
  zone `a:<n>` e `s:<n>`; il cursore manda il valore al rilascio), immagini decodificate con
  `media::decode_file` (WIC, ritaglio dal centro alla forma voluta, a 2×) e tenute finché il popup
  è aperto. Errori: una riga senza traceback (`lua::short`), nel log una volta finché non cambia;
  `bar` che fallisce → pill `⚠ nome` rossa e il popup con l'errore (anche un nome senza
  definizione, o i file che non si caricano). L'app: Lua caricato in `make_widgets` solo se un
  nome di `[layout]` non è un tipo di serie e ci sono file, e tenuto solo se un widget lo usa;
  ricaricato con la config (il watcher guarda anche le sottocartelle, il timbro conta i `.lua` di
  `widgets\` e `widgets\lib\`); `rustybar check` li carica e dice gli errori e i nomi Lua.
  `examples/widgets`: `pomodoro.lua` (senza rete, timer solo mentre conta) e `weather.lua`
  (Open-Meteo con `rb.http` e `rb.json`, ogni 15 minuti).

Decisioni (con l'utente, 12/11/2026, per la finestra delle impostazioni; specifica proposta con i
costi misurati e confermata con le scelte consigliate):

- **Processo**: lo stesso eseguibile, `rustybar settings [pagina]`, in un processo suo che finisce
  quando la finestra si chiude (la barra non ne porta niente, 0 a riposo). Una sola finestra per
  cartella di config: un secondo `rustybar settings` la porta davanti sulla pagina chiesta. Si apre
  dal menu di `power` («Impostazioni di RustyBar…» al posto di «Config di RustyBar…»; il file resta
  raggiungibile dalla pagina Informazioni), da riga di comando e dal collegamento «Impostazioni di
  RustyBar» nel menu Start (scritto e tolto da `rustybar install`/`uninstall`).
- **Toolkit (A)**: il disegno dei popup (Direct2D e DirectWrite su DirectComposition, WARP), in
  stile Impostazioni di Windows 11: pagine a sinistra, schede arrotondate di righe con titolo,
  descrizione e controllo a destra, coi colori del tema della barra (chiaro, scuro, accento).
  Controlli nostri: interruttore, menu a tendina, numero con − +, cursore, colore, elenco di testi,
  editor del layout; lo scorrimento sposta un visual (DWM, niente ridisegno). I campi di testo sono
  un EDIT nativo sopra il campo mentre lo si modifica (IME, appunti, annulla, menu: di Windows).
  Niente accessibilità UI Automation fuori dai campi di testo. Esclusi i controlli comuni di Win32
  (aspetto da Windows 10, tema scuro incompleto) e WebView2.
- **Pagine**: Barra (altezza, margine, riserva, distanza dei popup; i giochi), Tema (palette coi
  campioni, colore principale, font da DirectWrite, misure, animazioni, colori sovrascritti),
  Layout (le tre colonne coi chip da trascinare, ×, «+ Aggiungi»: i tipi non ancora nella barra,
  un'altra istanza di un tipo, i widget Lua di `widgets\`), una pagina per widget del layout (uno
  schema di opzioni per tipo; i segreti nel Gestore credenziali da un campo nascosto, mai nei file;
  per un widget Lua le chiavi che ha e «Aggiungi opzione»), Informazioni (versione, cartelle,
  «Apri rustybar.toml», gli errori di `rustybar check` in cima).
- **Scrittura**: `toml_edit` (+~104 KB, condivide il parser di `toml`), sul posto, commenti e
  ordine tenuti; ogni modifica si scrive subito (niente Salva), i campi di testo a Invio o
  all'uscita dal campo; il file si rilegge prima se è cambiato da fuori; le chiavi stanno dove sono
  (`rustybar.local.toml` o `rustybar.toml`), il nuovo va in `rustybar.toml` salvo dentro una
  tabella del file locale; un valore rimesso al predefinito sparisce; un file che non si legge si
  mostra e non si scrive finché non è a posto. La barra si ricarica da sola (il suo watcher).
- **Ricarica più fine della barra**: alla ricarica si tengono i widget il cui tipo e le cui opzioni
  non sono cambiati (e lo stato Lua se nessun `.lua` è cambiato); si rifanno solo gli altri. Così
  un clic nella finestra non rilancia `claude.exe`, non riconnette Home Assistant, non rilegge
  Docker e i calendari.
- **Lavoro in tre fasi**, ognuna provata prima della successiva: (1) ricarica fine, finestra,
  controlli, pagine Barra, Tema, Layout, Informazioni e scrittura; (2) le pagine dei widget con gli
  schemi e i segreti; (3) `RUSTYBAR_MOCK` (copie in `examples/mock/settings/`, niente applicato) e
  `rustybar settings --shot <pagina> <file.png>` fuori schermo per il README.
- Fuori (solo macOS): AeroSpace, JankyBorders, SwipeAeroSpace, MiddleClick, Brewfile.

- Finestra delle impostazioni (fatta, 12/11/2026, come le decisioni sopra), `src/settings/`:
  `mod.rs` è il processo e la finestra: `rustybar settings [pagina]` (`bar`, `theme`, `layout`,
  `about`, o il nome di un widget del layout: `w:<nome>`); un mutex per cartella di config
  (`Local\RustyBar-settings-<hash>`), la finestra ha la proprietà `RustyBarDir` con lo stesso hash:
  un secondo processo la trova con `EnumWindows`, le manda la pagina con `WM_COPYDATA` (la firma
  `RBM1` di `ipc`) dopo `AllowSetForegroundWindow` ed esce. `process::setup` come la barra
  (sotto il normale, EcoQoS). Finestra `WS_OVERLAPPEDWINDOW` + `WS_EX_NOREDIRECTIONBITMAP`, 1040×760
  DIP al centro dello schermo col mouse, minimo 760×520; barra del titolo nei colori della pagina
  (`DwmSetWindowAttribute` 20, 35, 36: scuro, colore del titolo e del testo). Albero
  DirectComposition: sfondo (una superficie di 1 pixel scalata), barra laterale, pagina sotto un
  clip (scorre con `SetOffsetY` e un'ease-out di 0,16 s eseguita da DWM; il cursore della barra di
  scorrimento è un visual che si trascina), menu, tinta dell'hover (nel visual della sua parte,
  così scorre con la pagina), il chip trascinato e la linea dove cadrebbe. Eventi in coda come
  nella barra, gestiti subito anche dentro il ciclo modale del ridimensionamento. Dopo ogni
  modifica la pagina si rifà dalla config e si ridisegna solo dove la scena è cambiata
  (`changed_region`, `BeginDraw` col rettangolo, clip): un clic su un interruttore ridisegna
  l'interruttore. Il menu a tendina è un riquadro nella finestra con 12 righe visibili (si
  ridisegnano alla rotella: l'elenco dei font ne ha centinaia), tastiera e lettera iniziale.
  `edit.rs`: l'EDIT di Windows in una finestrella `WS_POPUP` di proprietà della finestra, sopra il
  campo, nei suoi colori (`WM_CTLCOLOREDIT`), col font Segoe UI Variable; sottoclassato per
  Invio, Tab, Esc e rotella; `EN_KILLFOCUS` tiene il testo; la barra del titolo resta attiva
  mentre l'EDIT ha la tastiera (`WM_NCACTIVATE`). `form.rs`: `Form` costruisce una `Page` (scena +
  `Do` per zona: `Run`, `Menu`, `Field`, `Slide` con l'anteprima mentre si trascina, `Chip`) con
  righe e controlli in stile Windows 11 (`Look` dai colori del tema: schede un tono sopra lo
  sfondo); `go`/`open` per cambiare pagina o aprire un file; `confirmed` per il secondo clic.
  `pages.rs`: barra laterale (la sezione Widget coi widget del layout; scorre con la rotella se la
  finestra è bassa) e le pagine Barra, Tema, Layout, Informazioni; `schema.rs`: la pagina di ogni
  widget (dove sta, o «Non nella barra»; un'istanza in più si elimina al secondo clic) con lo
  schema del suo tipo: `clock` (date, timezone, open), `workspaces` (single), `media` (paused,
  cover, cover_colors), `system` (interval, gpu, soglie a cursore), `claude` (dir, color, path,
  soglie), `home` (server, token, plancia, gruppi di tessere come chip, stampante), `events` (i
  calendari a due piani: nome che sposta anche il segreto, colore, indirizzo segreto, cestino;
  soglie, interval, open), `docker` (la pipe); le chiavi che lo schema non conosce, e tutte quelle
  di un widget Lua, per tipo del valore, con «Aggiungi un'opzione» (`chiave = valore` in TOML, o
  testo). Segreti: `secret::set`/`delete` (`home-assistant`, `events-<nome>`), con i dati finti un
  insieme in memoria. `store.rs` (da quello della versione macOS): letture fuse, scritture sul
  posto senza mai lasciare il file vuoto (`write_in_place`: niente rinomina, un link resta un
  link), `push_table` anche dentro un array scritto in linea. Con `RUSTYBAR_MOCK` il processo mette
  `RUSTYBAR_CONFIG_DIR` su `<mock>\settings` all'avvio: config, `check`, Lua e log seguono quella.

## Nuove proposte (decise con l'utente il 12/11/2026)

Confrontata di nuovo la versione macOS (`ede268a`) e misurato con una sonda di sole letture
(con WoW aperto); la nota completa coi costi è nelle note condivise della sessione
(`proposte-2026-11-12.md`). L'utente: «fai tutto nell'ordine che ritieni corretto». Per le scelte
lasciate aperte valgono quelle consigliate: soglie della temperatura della GPU 85/95 °C se il
driver non le dice, la copertina nella pill come opzione spenta, il ping verso l'esterno come
opzione spenta. Ordine: le piccole prima.

1. **Crash** (fatto, `crash.rs`): `panic::set_hook` scrive thread, file, riga e messaggio nel log
   (`log::write_failing`, con `try_lock`: il thread che fallisce può avere il lock) e rilancia la
   barra con gli stessi argomenti; un fault in una chiamata di Windows passa da
   `SetUnhandledExceptionFilter` (codice e `rustybar+offset`; non rilancia dopo uno stack
   overflow). Il nuovo processo riceve `RUSTYBAR_AFTER` (il pid da aspettare, fino a 5 s, prima
   del mutex; tolto dall'ambiente) e `RUSTYBAR_RESTARTS` (gli orari delle ripartenze): al massimo
   3 in 10 minuti. La finestra delle impostazioni scrive il motivo e non riparte. Non
   `RegisterApplicationRestart` (solo dopo 60 s, e Segnalazione errori può mostrare una finestra
   che ruba il primo piano). Per provarlo: `RUSTYBAR_TEST_CRASH=1` nella barra di prova, poi
   `rustybar msg crash` (senza la variabile il comando non esiste). Provato: ripartita in ~110 ms
   tre volte, alla quarta «failed 3 times in 10 minutes». Non provato: lo spazio riservato da
   una barra morta che aveva `reserve = true`.
2. **Nomi sbagliati** (fatto): `config::closest` (distanza di Levenshtein fino a un terzo delle
   lettere, o un prefisso di almeno 4) in `rustybar check` (`unknown widget "sistem" in [layout]
   (maybe "system"?)`), nel log e nella pagina Layout (il chip rosso «sistem» con «→ system»
   nell'accento, che lo sostituisce; provato con `settings --shot layout`).
3. **`system`, la GPU** (fatto): soglie della temperatura in °C (`temp_warning`, `temp_critical`;
   senza, 10° sotto quella che il driver dice alta e quella, altrimenti 85 e 95: la Radeon
   di qui dice `TemperatureWarning = 0`, e prima la temperatura non cambiava mai colore); nel
   popup accanto a «GPU» temperatura e ventola (`2274 rpm`, «ventola ferma» a 0 giri con un
   massimo noto), accanto al valore la frequenza del core, nella memoria video quella della
   memoria. `Gpu::perf` (`ADAPTERPERFDATA`, la chiamata della temperatura: temperatura, ventola,
   memoria) e `Gpu::clock` (`NODEPERFDATA` del motore 3D, ~0,28 ms nel driver: solo a popup
   aperto, all'apertura e ogni 2 s). Il campo `Power` no: decimi di percento di cosa, non è lo
   stesso per ogni driver (qui 193 sotto carico). Nella pagina del widget le due soglie. Provato:
   barra di prova con un gioco aperto, GPU al 98%, 88 °C in giallo, 2274 rpm, 1666 MHz, memoria
   1796 MHz; dati finti (`clock`, `memory_clock`, `fan`, `fan_max` in `system.json`).
4. **`audio`, il microfono** (fatto): nella pill (e nella compatta) il microfono barrato rosso se
   l'ingresso predefinito è muto, altrimenti il microfono in `primary` se un'app lo usa, e la
   fotocamera (`E714`) se un'app la usa; nel popup sotto i volumi «Microfono in uso da Discord».
   `widgets/privacy.rs`: le chiavi `ConsentStore\microphone` e `\webcam` di HKCU (una per app
   pacchettizzata, il nome dalla famiglia del pacchetto; una per eseguibile sotto
   `NonPackaged`, il percorso con `#`, il nome dalla versione come `front_app`), in uso con
   `LastUsedTimeStart > 0` e `LastUsedTimeStop = 0`; ognuna con `RegNotifyChangeKeyValue`
   (albero, `LAST_SET | NAME | THREAD_AGNOSTIC`) su un evento suo, riarmato solo quello che è
   scattato (riarmarne uno ancora in attesa ne aggiungerebbe un altro). Aperte con le callback
   di Core Audio a barra visibile, chiuse senza. `privacy = false` le toglie. `signaled_at`
   riconosce l'handle (evento, timer della batteria, i due del registro) invece dell'indice.
   Misurato: barra di prova con `audio`, a riposo per 30 s, 4,3 MB privati, 0 ms di CPU,
   0,10 risvegli/s, 6 thread. Non provato: un'app che usa davvero il microfono (qui nessun
   ingresso attivo) e la fotocamera.
5. **`power`** (fatto): mentre Windows Update aspetta un riavvio (la sottochiave `RebootRequired`
   di `HKLM\…\WindowsUpdate\Auto Update`) un puntino `●` in `peach` accanto al logo, piccolo, e
   in cima al menu «Riavvia per completare gli aggiornamenti» (`E895`, apre
   `ms-settings:windowsupdate`). `RegNotifyChangeKeyValue` sulla sola chiave e sui soli nomi
   delle sottochiavi (i valori che Windows Update riscrive non svegliano), chiusa senza barre
   visibili. Dati finti: `update` in `power.json`. Provato coi dati finti; sulla barra di prova
   la chiave si apre e la sorveglianza parte senza errori. Non visto: la sottochiave che compare
   davvero (qui non c'è un aggiornamento in attesa).
6. **`network`** (fatto): `ping = "<host>"`, spento di predefinito: a popup aperto la riga «Ping»
   nei dettagli (si copia come le altre), un secondo `Ping` ICMP come quello al router, il primo
   appena l'indirizzo c'è e poi ogni 5 s. Un IPv4 subito, un nome risolto una volta su un thread
   suo (`ToSocketAddrs`, il primo IPv4) e tenuto per i popup dopo. `handles`/`signaled_at`
   riconoscono l'handle (timer, ping al router, ping all'host). Nella pagina del widget «Ping
   verso». Dati finti: `host_ping` in `network.json`, `ping = "1.1.1.1"` nella config finta.
   Provato: `one.one.one.one` dalla barra di prova, 12 ms.
7. **`media`** (fatto): (a) l'album (`AlbumTitle`) nel popup, fra artista e app (quattro righe
   nell'altezza della copertina; tre senza album). (b) Il titolo nello spazio libero:
   `Widget::wide` (una forma più larga, per i widget del gruppo centrale; `media` taglia il
   titolo a 520 e l'artista a 320 invece di 240 e 160), misurata solo se più larga della piena
   (in cache come `nome\0wide`); `widen` in `app.rs` la usa quando nessuna pill ha ceduto e i
   lati entrano accanto al centro largo, con `SLACK` la prima volta (`Fitted::center_wide`).
   Provato coi dati finti e un titolo lungo stringendo col margine: 656 px larga, 401 piena col
   margine a 900, 156 compatta più stretta, e di nuovo larga. (c) `pill_cover = true` (spenta):
   `Item::image` (una figura quadrata nella pill, disegnata con `popup::draw_image` nella barra
   e nel popup ⋯, nella firma del testo col suo id) al posto di ♪ mentre suona (in pausa il
   segno della pausa); la copertina a 40 px decodificata a ogni brano sul thread del pool dove
   arrivano le proprietà (`Shared::small_cover`, col numero di sessione). Misurato col player
   finto: +0,2 MB privati, +3,7 MB di working set condiviso (WIC), 8 thread invece di 6 (il pool,
   che si svuota da solo); per questo spenta. Nella pagina del widget «Copertina nella pill».
   Dati finti: `album`, e `pill_cover = true` nella config finta.
8. **Mixer nel popup di `audio`** (fatto, `widgets/mixer.rs`): sotto i volumi «App», le sessioni
   dell'uscita predefinita non scadute raggruppate per eseguibile (nome dalla versione;
   «Suoni di sistema» in fondo; un processo che non si apre col nome della sessione se è un
   nome, altrimenti fuori), per nome, al massimo 8; riga con l'altoparlante (clic = muto), nome
   (grigio se non suona), cursore (`app-volume:i`) e percentuale; impostate tutte le sessioni,
   lette dalla prima. Solo a popup aperto: `IAudioSessionManager2` dell'uscita,
   `IAudioSessionNotification` per le nuove (dopo un primo `GetSessionEnumerator`, come vuole
   Core Audio), `IAudioSessionEvents` per volume, stato e fine; flag ed evento del widget. Al
   cambio dell'uscita predefinita si rifà. `mixer = false` lo toglie. Dati finti: `apps` in
   `audio.json`. Provato: sulla barra di prova le app vere (un gioco, i launcher, i suoni di
   sistema). Non provati dal vivo: i cursori e il muto delle app (cambierebbero i volumi).
9. **Widget `devices`** (fatto, `widgets/devices.rs`, fuori dal layout predefinito): le batterie
   dei dispositivi Bluetooth. Ogni nodo presente di `BTHENUM`/`BTHHFENUM`/`BTHLE`/`BTHLEDEVICE`
   con la proprietà della batteria (quella di `audio`, `{104EA319…} 2`), uno per contenitore
   (`DEVPKEY_Device_ContainerId`); dal nodo del dispositivo dello stesso contenitore (`…\Dev_…`)
   il nome (il nodo del profilo si chiama «… Hands-Free HF»), il tipo
   (`DEVPKEY_DeviceDisplay_Category`, `{78C34FC8…} 90`: `Input.Keyboard`,
   `Communication.Phone`, `Communication.Headset.Bluetooth`… → l'icona) e se è collegato
   (`DN_DEVICE_DISCONNECTED` = `0x02000000` nello stato: c'è anche coi dispositivi abbinati ma
   lontani, spenti o sul loro ricevitore 2,4 GHz). Pill solo con un collegato a `low` (20) o
   meno, `always = true` tutti i collegati: icona del tipo, batteria e percentuale (gialla al
   20%, rossa al 10%, i colori di `audio`); compatta senza numeri. Popup (300 DIP): «Dispositivi
   Bluetooth», collegati prima e in grassetto, gli altri grigi con «Non collegato» sotto il nome
   e l'ultimo livello; «Impostazioni Bluetooth» (`ms-settings:bluetooth`). Letture: tutti i nodi
   (~10 ms per 75) all'avvio, all'apertura del popup, al risveglio, quando una barra torna e un
   secondo dopo l'ultimo `WM_DEVICECHANGE` con `DBT_DEVNODES_CHANGED` (nuovo `Signal::
   DevicesChanged` dalla finestra di controllo; un `wake::Timer` usa e getta, fermato e
   rifatto); i nodi noti ogni 5 minuti solo con una barra visibile, nessun gioco della lista e
   almeno un dispositivo. Opzioni nella pagina del widget («Nella barra da», «Sempre nella
   barra»). Dati finti: `devices.json`, nella barra finta. Provato qui: cinque dispositivi
   trovati coi nomi e i tipi giusti (cuffie, due tastiere, due telefoni), tutti non collegati;
   barra di prova a riposo per 30 s: 3,7 MB privati, 0 ms di CPU, 0,13 risvegli/s. Non visto: un
   dispositivo che si collega (la pill che compare, il giro dopo `WM_DEVICECHANGE`), un mouse e
   un controller veri.
10. **Per schermo** (fatto): `[bar] screens` (`all`, predefinito, o `primary`: `monitors()` dice
    anche quale è il principale, `MONITORINFOF_PRIMARY`, e `build_bars` salta gli altri; un
    valore diverso è un errore di `rustybar check`) e `[bar] always_compact` (i widget nominati
    nella forma compatta su ogni schermo: `App::always_compact` sostituisce la loro pill piena
    con la compatta prima di `fit`, in cache come `nome\0compact`; `msg query` non li dice
    compatti). Nella pagina Barra «Schermi» (menu) e «Sempre compatti» (elenco). Provato:
    `always_compact = ["system"]` dà la pill di 80 px invece di 246, tolto torna piena. Non
    provato: `primary` con due schermi accesi (qui l'altro è spento).
11. **Lua** (fatto): `rb.home` come su macOS: `state(id)` → `{ state, attributes, name }` o
    `nil`, `online()`, `call(dominio, servizio, { entity_id = … })` → `true` se c'è il widget
    `home` (che lo manda come i servizi delle tessere, `POST /api/services`; coi dati finti
    niente), e l'evento `home_assistant` (`args.entity`, `args.state`; senza argomenti quando
    torna in linea), da `app::Event::Trigger` come `rb.trigger`. In `home.rs` un `Bridge` in un
    `thread_local` (thread principale): `present`, `online`, gli stati seguiti, le chiamate in
    coda e l'evento del widget; `publish` dopo ogni evento della WebSocket e alla caduta, il
    `Drop` di `Home` lo svuota. `[widgets.home] watch = [...]`: entità seguite oltre a tessere e
    stampante, per i widget Lua. `lua::to_json` (tabella 1…n → array, altre → oggetto). Nel
    popup `rb.ui.toggle` (etichetta e interruttore stile Windows 11, `on`, `on_change(valore)`
    come lo slider, zona `t:<n>`) e `rb.ui.disclosure` (nel preludio Lua: riga col titolo e il
    chevron `E70D`/`E76C` a destra, i pezzi solo con `open`, `on_click` per il widget, come su
    macOS). Riga con icona (`row` con `rb.ui.icon`) e griglia a n colonne (`tiles { columns }`)
    c'erano già. `examples/widgets/lights.lua`: le luci scelte con un interruttore ciascuna,
    senza timer (ascolta `home_assistant`). Test: interruttore e sezione, `rb.home` senza `home`,
    il ponte con gli stati. Provato sulla barra finta: pill «lampadina 2», popup con tre
    interruttori. Non provati: un Home Assistant vero (`call` vero, l'evento dal vivo).
12. **Impostazioni di Home Assistant** (fatto, `schema::home`): per ogni gruppo il titolo con ↑ ↓
    e «Togli»; ogni tessera su una riga (l'id, sotto il nome del server se caricato) con «nome
    suo», «icona», ↑ ↓ e il cestino (`put_tile`: l'id da solo, o una tabella in linea `{ id,
    name, icon }`); «Aggiungi una tessera» col menu «Dal server…» (solo i domini che una tessera
    mostra) e l'id a mano; «Entità del server»: «Carica»/«Ricarica» chiede `GET /api/states` col
    token su un thread di lavoro (con i dati finti gli stati di `home.json`), poi
    `settings::refresh` (un `PostMessage` di `WM_APP + 1` alla finestra, `Ev::Refresh`) rifà la
    pagina; l'elenco resta finché la finestra è aperta. «Per i widget Lua»: `watch` (elenco e
    menu). `store::swap` (`swap_in`) scambia due elementi di un array o di un `[[…]]`: ogni posto
    tiene spazi e commenti, le tabelle si scambiano anche la posizione nel file (test).
    `Control::Gap` tiene il posto della freccia che una riga non ha, così i controlli restano in
    colonna. Provato dal vivo con la finestra di prova fuori schermo su una copia dei dati finti:
    «Carica» → «13 entities», ↓ sulla prima tessera la scambia con la seconda nel file. Nota per
    le prove: `FindWindow("RustyBarSettings")` può trovare la finestra vera dell'utente se è
    aperta: cercare quella di prova per titolo («… (made-up data)») o controllare prima che non
    ce ne sia un'altra (questa volta quella dell'utente era chiusa dalle 19:15). Non provati: la
    scelta dal menu «Dal server…» (il menu si apre come gli altri), un server vero.
13. **Widget `display`** (fatto, `widgets/display.rs`, fuori dal layout predefinito): pill con
    l'icona dello schermo, la frequenza del principale (la sorgente in 0, 0) e «HDR» se acceso;
    compatta la sola frequenza. Popup (320 DIP): un riquadro per schermo con nome
    (`monitorFriendlyDeviceName`) e risoluzione, «Frequenza di aggiornamento» come chip (le
    frequenze di `EnumDisplaySettingsW` alla risoluzione attuale, quattro per riga; clic →
    `ChangeDisplaySettingsExW` con `CDS_UPDATEREGISTRY`), HDR con l'interruttore se lo schermo
    lo supporta (lo stato HDR di 24H2, `GET_ADVANCED_COLOR_INFO_2` = 15 e `SET_HDR_STATE` = 16,
    strutture scritte a mano perché windows-rs non le ha; prima di 24H2 lo stato del colore
    avanzato, 9 e 10), la luminosità via DDC/CI col cursore (inviata al rilascio, su un thread);
    «Impostazioni dello schermo». La configurazione (`QueryDisplayConfig` e i nomi) all'avvio e a
    `Signal::Displays` (nuovo: l'app lo manda dopo aver rifatto le barre per `WM_DISPLAYCHANGE`)
    e al risveglio; frequenze e luminosità solo all'apertura del popup, la luminosità su un thread
    di lavoro (`GetMonitorBrightness` per monitor fisico, il risultato con l'evento del widget).
    `brightness = false` la toglie. A riposo niente: nessun timer né thread. Dati finti
    `display.json` (nella barra finta). Provato in sola lettura sui due schermi di qui: nomi,
    risoluzioni, frequenze (60/120/240 e otto per il secondo), HDR acceso sul principale e
    spento sul secondo (lo stato di 24H2 risponde), luminosità di 2 monitor in 120–126 ms. Non
    provati, perché cambierebbero gli schermi: il cambio di frequenza, HDR acceso e spento, la
    luminosità scritta; e se `WM_DISPLAYCHANGE` arriva al cambio di HDR.
14. **Giro di prove** (fatto) dei «non provati» fattibili da qui:
    - Avvio all'accesso: confermato dal log vero e dall'evento 7001 (il 12/11 accesso alle
      10:13:07, barra alle 10:13:50); il collegamento «Impostazioni di RustyBar» c'è nel menu Start.
    - Tema chiaro: tutti i popup della barra finta con `catppuccin-latte`, si leggono (i nuovi
      compresi: mixer, batterie, schermi, Windows Update, ventola della GPU).
    - Scale 125/150/200%: le pill di sinistra (logo, puntino, desktop, schermi) nitide.
    - Un widget Lua nel ⋯ (margine 2500): si vede e funziona come gli altri.
    - `rb.exec` in timeout: trovato e corretto un difetto. Il timeout uccideva solo `cmd.exe`; il
      `ping` che aveva lanciato teneva aperte le pipe e la risposta arrivava a 44 s invece di 30.
      Ora `process::run` mette ogni comando anche in un job suo, annidato in quello globale, e
      allo scadere fa `TerminateJobObject` di quello: risposta a 30 s, nessun processo rimasto
      (vale anche per `claude`).
    - `claude.exe` appeso (un eseguibile di prova che dorme): timeout a 20 s, nessun processo
      rimasto. Trovato e corretto: senza risposta la pill diceva «✻ Accedi» come senza account;
      ora `✻ –`, e il popup «Claude Code non ha risposto: riprova all'apertura del popup».
    - Non fatti da qui: i clic sui popup finti aperti sullo schermo (coprirebbero lo schermo
      mentre l'utente gioca); coperti dai percorsi di codice e dai test.

    Da provare con l'utente (solo lui può, o cambierebbe il PC): la finestra delle impostazioni
    col mouse vero (hover, menu, campo di testo, trascinamento, il menu «Dal server…» di Home
    Assistant); un secondo desktop virtuale (`workspaces`); «Disinstalla» da Impostazioni > App;
    un cambio di DPI e del tema chiaro/scuro di Windows al volo; HDR acceso e spento e un cambio
    di frequenza da `display` (e se arriva `WM_DISPLAYCHANGE`); la luminosità scritta via
    DDC/CI; un dispositivo Bluetooth che si collega (`devices`); un'app che usa il microfono o
    la fotocamera (`audio`); i cursori del mixer su app vere; un crash della barra installata
    con `reserve = true` (lo spazio riservato che resta); `screens = "primary"` con due schermi.

## Batterie di mouse, tastiere e controller (decise con l'utente il 13/11/2026)

L'utente vuole nella barra la batteria del mouse, della tastiera e dei controller collegati, tutti
se sono più d'uno. Qui: Razer Naga V2 Pro (cavo, ricevitore HyperSpeed, Base Station), Razer
BlackWidow V4 Pro 75%, Logitech MX Master 4 (Bolt), ROG Falcata (tastiera divisa, ricevitore
SpeedNova 8K), una Keychron (Ultra-Link 8K), uno Steam Controller (2026, puck), più tastiere in
Bluetooth. Sonda di sola lettura (solo comandi GET) e costi nella nota condivisa
`batterie-hid-2026-11-13.md`; nessuna interfaccia ha la batteria standard HID (pagina 0x06/0x20):
ogni produttore ha il suo protocollo. Proposta confermata così, più i controller PlayStation:

- **`devices` si allarga** (non un widget nuovo): oltre al Bluetooth, i mouse, le tastiere e i
  controller dietro un ricevitore o un cavo. Razer (feature report di 90 byte, `07 80` e `07 84`),
  Logitech (HID++ 2.0 attraverso il ricevitore, `0x1004` o `0x1000`; il nome dal ricevitore anche
  mentre il dispositivo dorme), ROG (ricevitori SpeedNova/Omni: `12 01` per la tastiera, le due
  metà della Falcata, dedotto da una lettura e confermato dall'utente; `12 07` per il mouse, come
  G-Helper), Steam Controller (il rapporto 0x43 che arriva da solo ogni ~3,5 s), DualSense e
  DualShock 4 (il byte della batteria nel rapporto d'ingresso, come SDL; non provati: qui non ce
  n'è uno), Xbox (XInput, quattro livelli; non provato). Fuori: la Keychron sul ricevitore (nessun
  protocollo documentato: niente comandi tirati a indovinare), in Bluetooth si vede già.
- **Costo**: tutto su un thread di lavoro che poi finisce, `hid.dll` e `xinput1_4.dll` caricate
  solo per il giro e lasciate. Scoperta (solo le interfacce dei produttori noti, scelte dal
  percorso prima di aprirle: aprirle tutte costa ~330 ms) all'avvio, quando cambiano i dispositivi,
  al risveglio e quando una barra torna; letture ogni 5 minuti con una barra visibile e nessun
  gioco della lista, e all'apertura del popup. Misurato con la sonda: scoperta 132 ms, 10 ms di
  CPU; un giro 3,5 ms di CPU (0,6 s d'attesa, 3,4 s con lo Steam Controller).
- **Barra**: `pinned = ["mouse", "keyboard", "controller"]` tiene quei tipi sempre nella pill, uno
  per dispositivo collegato; gli altri solo a `low`; `always = true` resta (tutti). La batteria in
  carica con l'icona della carica, i livelli approssimati (XInput) senza numero, la Falcata con la
  metà più scarica. `receivers = false` lascia il solo Bluetooth.
- **Chi dorme resta** (chiesto dall'utente il 13/11/2026, dopo averle viste sparire): un
  dispositivo dietro un ricevitore o un cavo che non risponde (una tastiera lasciata stare, un
  mouse spento) ma che ha risposto da quando la barra è partita resta nella pill, tutto in grigio
  (`subtext`) col livello di allora; nel popup «Non risponde». Uno mai letto da quando la barra è
  partita no (nessun livello da mostrare); il Bluetooth non collegato no (il suo livello può
  essere di giorni prima).
- **Aspetto** (chiesto dall'utente il 13/11/2026: «più accattivante e con più informazioni,
  altrimenti non so quale dispositivo è quale»; scelto da una bozza fra due pill, «A» e il popup
  a riquadri). Pill: tutti in una, con una linea sottile fra uno e l'altro; per ognuno l'icona del
  tipo, il nome corto in `subtext` (il modello senza il produttore e le parole che hanno tutti:
  `Naga`, `BlackWidow`, `Falcata`, `MX Master 4`, `Steam`, `Steam 2`; o quello dell'utente,
  `names = { "Razer Naga V2 Pro" = "…" }`, dalla pagina del widget), una barretta col livello
  e il numero, il fulmine giallo in carica. Colori a scala su tutto il livello (chiesto
  dall'utente: la MX Master 4 al 25% era verde, «ma è quasi scarico»; `level_name`): verde sopra
  il 50%, giallo fino al 25%, `peach` (arancio) fino al 10%, rosso sotto; il numero nel colore
  della barra da giallo in giù, nel colore del testo quando è verde; nel popup ogni metà della
  Falcata col suo. Il riquadro tinto e la pill per chi scende sotto `low` restano a 20. Compatta: icona e
  barretta. Popup (324 DIP): un riquadro per collegato (o addormentato) con il tipo in un tondo del
  suo colore, il nome intero, produttore · collegamento · in carica (o «non risponde · letto alle
  HH:MM»), il livello grande e una barra (due per le metà della Falcata, «Metà 1/2»), il riquadro
  tinto quando è basso; poi «Non collegati» in righe brevi.
- **Doppioni**: un dispositivo non collegato da una parte (Bluetooth, o il ricevitore senza il suo
  dispositivo) e collegato dall'altra, stesso produttore e tipo, si mostra una volta.

Fatto (13/11/2026), com'è fatto:

- `widgets/hid.rs`: `pass(known, rediscover)` sul thread di lavoro. `hid.dll` caricata con
  `LoadLibraryExW` per il giro (`Api`, puntatori con `function`, come `netapi32` in `power`) e
  lasciata alla fine; `xinput1_4.dll` lo stesso. Scoperta: `CM_Get_Device_Interface_List` della
  classe HID, poi solo i percorsi con `vid_1532`, `vid_046d`, `vid_0b05`, `vid_28de`, `vid_054c` o
  `&0002054c` aperti con accesso 0 (`HidD_GetAttributes`, `HidP_GetCaps`, l'ID dei rapporti
  d'uscita, il nome del prodotto, il contenitore da `DEVPKEY_Device_InstanceId` →
  `DEVPKEY_Device_ContainerId`); le collezioni (solo i percorsi, nessun handle) restano fino al
  prossimo cambio di dispositivi. `Port`: una collezione aperta in overlapped con una lettura in
  attesa alla volta (buffer e `OVERLAPPED` in un `Box`, annullata al `Drop`). Razer: per
  contenitore, le collezioni con il feature report di 91 byte; transazione 0x1F, poi 0x3F e 0xFF;
  le risposte di un altro programma (comando diverso) ignorate, stato 1 = occupato (5 ms e
  riprova, al massimo 10 volte), 4 = il ricevitore senza il dispositivo (non collegato), 5 = non
  lo sa (non è uno con la batteria); tipo dal PID (sotto 0x0100 mouse, fino a 0x04FF tastiera, il
  resto no: basi, tappetini, luci, case, cuffie); in carica → collegamento «Cavo». Logitech: per
  contenitore le collezioni HID++ corta (FF00/0001) e lunga (FF00/0002), lette insieme con
  `WaitForMultipleObjects`; il software id 0x0A; per un ricevitore (nome del prodotto con
  «receiver») gli slot 1–6 con `IRoot.getFeature(0x1004)` → `getStatus` (o `0x1000`), nome e
  tipo dai registri del ricevitore (Bolt `B5 5n`/`B5 6n` con 1; Unifying e Lightspeed `B5 2n`/
  `B5 4n`, non provati); uno slot senza nome nei registri è vuoto; un dispositivo che non è un
  ricevitore all'indirizzo 0xFF col nome da `0x0005`. ROG: solo i contenitori con una collezione
  mouse o tastiera (mai le luci della scheda madre); sulle collezioni vendor da 64 byte, rapporto
  2 → `12 01` (byte 6 il livello, byte 11 l'altra metà se il byte 7 è 2; 0 = dorme), rapporto 3 o
  0 da 65 byte → `12 07` (byte 5, carica al byte 10); `FF AA` = niente lì; senza nome (il
  ricevitore) lo prende dal Bluetooth dello stesso produttore e tipo, altrimenti «Tastiera ROG».
  Valve: le collezioni FF00/0001 di 28DE:1302–1304, `HidD_SetNumInputBuffers(512)`, fino a tre
  pause da 1,5 s e svuotate; uno slot che non ha mandato niente non ha un controller. Sony: la
  collezione gamepad (pagina 1, uso 5), un solo rapporto; Bluetooth dal percorso (`{00001124` o
  `&0002054c`): il rapporto corto 0x01 di Bluetooth è ignorato (non ha la batteria). XInput:
  `XInputGetBatteryInformation` dei quattro posti, cablato e scollegato no; livelli 5/30/60/100
  per disegnare la batteria, nel popup «Scarica/Bassa/Media/Piena» e nella pill senza numero.
  `merge`: un dispositivo per chiave, quello collegato e in carica prima.
- `widgets/devices.rs`: `Paired` (Bluetooth, come prima, più il produttore dal `VID&` del nodo
  HID dello stesso contenitore: Razer in Bluetooth dice 0x068E) e `Found` (ricevitori e cavi) →
  `Device` (`combine`). Il lavoro: `Work` (le collezioni e il risultato in un `Mutex`) e un
  `wake::Wake` tra gli handle; un giro alla volta (`reading`), uno chiesto intanto parte dopo
  (`again`, con la nuova scoperta se uno dei due la voleva); thread `devices` da 256 KB di stack.
  `last`: l'ultimo livello di ogni chiave, per chi dorme (`Device::asleep`: non risponde ma è in
  `last`; `in_pill` lo tiene, `colors` lo attenua); mai letto → «–». `combine`: dentro un
  produttore e un tipo (non `Other`), se qualcuno è collegato gli altri spariscono, se nessuno ne
  resta uno (con un livello, prima quello del ricevitore); i controller XInput tanti quanti gli
  Xbox collegati in Bluetooth spariscono. Timer di 5 minuti con qualcosa da leggere (Bluetooth o
  collezioni o dispositivi trovati). Opzioni: `low`, `always`, `pinned` (`mouse`, `keyboard`,
  `controller`, `other`), `receivers`. `Options::strings` in `config.rs`. Pagina del widget:
  «Sempre nella barra, per tipo» (tre interruttori che scrivono `pinned`, `schema::pinned`),
  «Nomi nella barra» (`schema::names`: un campo per nome dato, il cestino, e «Aggiungi un nome»
  con `nome completo = nome corto`) e «Ricevitori e cavi». La barretta della pill: `ui::Drawn`
  (`Item::gauge`, `Item::rule`), disegnata in `bar::paint_text` e in `pill_shapes` del popup ⋯
  da `ui::drawn` (allineata ai pixel). Nome corto: `short_name`. Dati finti: `link`, `charging`,
  `second`, `approx`, `asleep`, `read`, `battery` assente = mai letto; nella barra finta `pinned = ["mouse", "keyboard", "controller"]`. Screenshot del
  README rifatti: `devices.png`, `bar-right.png`.

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
  (il logo nel colore del testo scuro sul desktop) e una scala diversa dal 100%. Committato e
  pushato (`34a8a4e`) e installato, con il permesso dell'utente.
- 11/11/2026, **dati finti e screenshot** (punto 8; dettagli in «Dalla versione macOS»): i dati
  finti degli otto widget che non li avevano, `msg shot`, `RUSTYBAR_LOCALE`, e 12 PNG in
  `docs/screenshots` (le due estremità della barra e dieci popup, in inglese, a scala 2) nel
  README, fatti dalla barra finta in `examples\mock`, rimasta accesa sopra quella vera solo il
  tempo degli scatti. Viste guardando i PNG, in inglese saltate fuori e corrette: la data lunga
  nel popup di `clock` tagliata (ora senza l'anno quando non ci sta: il modello della regione,
  `LOCALE_SLONGDATE`, senza le `y`), «Container» per i container senza progetto di `docker`
  (ora «Containers»; e le percentuali senza spazio, come negli altri widget), `2,4 GHz` di
  `network` con la virgola fissa (ora col separatore della regione). Test: ogni JSON di
  `examples/mock` si legge. Binario ~1,93 MB. Non provati: i clic sui popup finti (cursori,
  muto, selettori del player e il clic su un desktop) e la barra finta col tema chiaro.
  Committato e pushato (`002d86b`).
- 12/11/2026, **widget in Lua** (punto 9; decisioni e com'è fatto in «Dalla versione macOS»).
  Prima del codice una sonda (stesso profilo della barra) per i costi da proporre all'utente;
  confermata la specifica così com'era. `process::run` preso da `claude.rs` (stesso
  comportamento) per `rb.exec`. Test: 15 nuovi (sandbox, 250 ms, 16 MB, colori, opzioni, pill,
  errori, eventi e trigger, `every` e `self.every`, popup con riquadri e cursore, `rb.exec` vero,
  `rb.json`, icone): 68, più 3 ignorati che vanno in rete. Barra di prova sullo schermo
  principale (`reserve = false`, `margin = 1200`) coi due esempi: `check` dice i nomi Lua e un
  nome sbagliato; pill e popup
  guardati con `msg shot` (il meteo vero di Roma da Open-Meteo); `msg trigger hello who=Alex`
  cambia la pill di un widget che ascolta; un `.lua` salvato ricarica la barra (un salvataggio, uno
  o due ricaricamenti); un `bar` che fallisce mostra `⚠ hello` in rosso e l'errore nel popup.
  Trovato e corretto: `net::request` chiedeva anche `deflate`, e Open-Meteo lo manda (WinHTTP
  falliva con `E_ABORT`): ora solo gzip, il calendario di Google di `events` si legge ancora (test
  ignorato `a_server_that_likes_deflate`). Misure a riposo per 60 s, barra di prova con un
  orologio: senza Lua 3,8 MB privati, 6,6 MB committed, 0,20 risvegli/s, 5 thread; con
  `pomodoro` (fermo) e un widget che ascolta un trigger 2,6 MB privati, 7,3 MB committed (+0,7),
  0 ms di CPU, 0,18 risvegli/s, 5 thread; col meteo dopo la sua richiesta HTTPS 5,2 MB privati,
  8,4 MB committed, 1,17 risvegli/s, 7 thread (i thread di WinHTTP, come per `events`). Caricare
  gli script: 1–6 ms; primo frame 36–46 ms. Binario ~2,66 MB (+0,73 MB: Lua e il suo codice).
  Non provati: un clic vero col mouse nei popup Lua (coperti dai test di `popup_input`), un
  `rb.exec` lungo che va in timeout, il tema chiaro, un widget Lua dentro il ⋯.
- 12/11/2026, **finestra delle impostazioni** (punto 10; decisioni e com'è fatta in «Dalla
  versione macOS»), in tre fasi. Fase 1, ricarica fine: barra di prova con `claude`, `home` (senza
  token), `system`, `clock`, `power` e un widget Lua; cambiato il formato di `clock` si rifà solo
  `clock`, margine e tema non rifanno nessun widget (`home` non riconnette, `claude` non riparte),
  un `.lua` cambiato rifà solo il widget Lua, `msg reload` rifà tutto. Trovato provando: la barra
  leggeva il file troncato da `fs::write` e per un attimo caricava il layout predefinito; ora la
  finestra scrive senza mai svuotarlo e la barra aspetta 150 ms dopo il cambio. Finestra di prova
  fuori schermo e senza primo piano (`RUSTYBAR_SETTINGS_TEST`) guidata con messaggi: − e + (un
  valore rimesso al predefinito sparisce), interruttore, menu a tendina da tastiera (palette),
  campo di testo (un gioco aggiunto in una tabella `[games]` nuova), trascinamento di un chip fra
  le colonne (una sola ricarica), «+ Aggiungi» un'altra istanza (`system_2` con `type`), ×,
  cursore (una scrittura al rilascio, si rifà solo `system`), «Aggiungi un'opzione» di un widget
  Lua, il segreto coi dati finti (né il Gestore credenziali né il file toccati). Mio errore
  durante le prove: un secondo clic sull'interruttore «Riserva lo spazio» ha fatto riservare la
  striscia alla barra di prova per ~1,7 s (17:00:07–09), con le finestre ingrandite che possono
  essersi spostate e tornate; l'utente giocava a WoW (il log della barra di prova lo dice in primo
  piano). Misure: barra di prova a riposo per 60 s dopo la fase 1 4,7 MB privati, 7,6 MB
  committed, 16 ms di CPU, 0,10 risvegli/s, 5 thread, 0 figli; finestra aperta (Tema, o Home
  Assistant coi dati finti) 7,5–8,1 MB privati, 12 MB committed, 0 ms di CPU in 20 s,
  0,10 risvegli/s, 6 thread, 0 figli; ~4,7 ms di CPU per modifica (10 clic su +: 47 ms); finestra
  mostrata 34–43 ms dopo l'avvio. Binario 3,11 MB (+0,45 MB: `toml_edit` e la finestra). Test: 78
  (10 nuovi: store sul posto e nei due file, numeri, secondo clic, regione cambiata, opzioni
  scritte a mano, la config finta della finestra). Screenshot in inglese fatti con `--shot`:
  `settings-layout.png`, `settings-home.png`, e `power.png` di nuovo (dice «RustyBar settings…»).
  Non provati: la finestra davanti con un mouse vero (hover, cursore a I, menu col mouse, EDIT che
  prende la tastiera e la barra del titolo che resta attiva, rotella, trascinamento del cursore
  della barra di scorrimento), il cambio di DPI, il tema chiaro e scuro al volo di Windows, il
  primo piano preso quando un secondo `rustybar settings` la richiama (provato che le passa la
  pagina ed esce in 70 ms, lasciando un processo solo), il collegamento nel menu Start (serve
  `rustybar install`), un segreto vero.
- 12/11/2026, **finestra vuota** (vista dall'utente dopo l'installazione: solo lo sfondo): i
  visual erano aggiunti con `AddVisual(v, true, None)`, che senza visual di riferimento mette il
  nuovo *sotto* i fratelli (`false` lo mette sopra): lo sfondo, aggiunto per primo con `false`,
  copriva barra laterale, pagina, menu, chip trascinato. Nelle prove non si vedeva (finestra fuori
  schermo, screenshot con `--shot`). Ora tutti con `false`, nell'ordine in cui vanno; controllato
  con `PrintWindow` sulla finestra di prova: pagina, barra laterale, barra di scorrimento e menu
  a tendina sopra la pagina. `rustybar install` e `uninstall` chiudono anche le finestre delle
  impostazioni aperte (`close_settings`, `WM_CLOSE` come la X): girano dallo stesso eseguibile, che
  finché è in uso non si può sostituire.
- 12/11/2026, **nuove proposte** (le 14 della sezione «Nuove proposte», fatte nell'ordine scelto:
  crash, nomi, GPU, microfono e mixer, Windows Update, ping, `media`, `devices`, per schermo,
  Lua, Home Assistant, `display`, giro di prove). Ognuna con build, clippy senza avvisi e test
  (87, più 3 ignorati), provata coi dati finti e dove si poteva su una barra di prova
  (`reserve = false`) o con la finestra delle impostazioni di prova; i dettagli e cosa non è
  provato sotto ogni punto. Screenshot del README rifatti dalla barra finta (`audio`, `system`,
  `network`, `media`, `power`, le due estremità della barra, `settings-home`) e due nuovi
  (`devices`, `display`). Misura finale, barra di prova col layout predefinito, due schermi
  accesi e un gioco che fa lavorare la GPU, a riposo per 60 s: 6,0 MB privati, 9,2 MB committed,
  47 ms di CPU, 3,8 risvegli/s, 5 thread, GPU 0, 0 figli, primo frame 58 ms; la build di prima
  nelle stesse condizioni 5,6 MB, 9,3 MB, 47 ms, 6,4 risvegli/s, primo frame 62 ms. Binario
  3,35 MB (+0,24 MB). Niente committato né installato.
- 13/11/2026, **batterie di mouse, tastiere e controller** (sezione omonima sopra): chiesti
  all'utente i dispositivi, sonda di sola lettura, proposta confermata più i controller
  PlayStation. Barra di prova (`reserve = false`, `pinned = ["mouse", "keyboard", "controller"]`)
  coi dispositivi veri: pill «mouse 100% (in carica) · tastiera 53% · controller 91%»; popup con
  la Naga sul cavo in carica, la Falcata 53% · 53% col nome del Bluetooth, lo Steam Controller,
  MX Master 4 e BlackWidow addormentati (una volta sola: prima la BlackWidow compariva due volte,
  dal Bluetooth e dal ricevitore, corretto con la regola di `combine`). Scoperta: 88 collezioni in
  ~210 ms sul thread di lavoro; un giro 2,2–3,7 s d'attesa (lo Steam Controller). Una ricarica con
  scoperta e giro: +12 cambi di contesto sul thread principale. Misure: a riposo per 60 s 4,6 MB
  privati, 8,5 MB committed, 0 ms di CPU, 0,08 risvegli/s, 6 thread, 0 figli (una barra di prova
  col solo orologio, appena avviata: 4,1 MB, 7,2 MB committed, 5 thread); 330 s con un giro del
  timer: 63 ms di CPU in tutto (orologio compreso), e il giro sveglia il thread principale ~7
  volte (contatori per thread). In quelle finestre lunghe c'erano ~780 cambi di contesto, ma a
  raffiche anche lontano dal giro (+52 in 10 s senza giro): non vengono dal giro, probabilmente il
  mouse dell'utente sulla striscia della barra di prova, che è sopra tutto. Binario
  3,40 MB (+54 KB; `hid.dll` e `xinput1_4.dll` non sono importate: caricate per il giro). Test:
  98, più 3 ignorati. Screenshot `devices.png` e `bar-right.png` rifatti dalla barra finta. Non
  provati: un mouse ROG sul ricevitore (qui solo la tastiera), DualSense, DualShock 4 e Xbox
  (nessuno collegato), i ricevitori Unifying e Lightspeed (i loro registri), la carica della
  Falcata (non so ancora quale byte la dice), un Logitech collegato via cavo (indirizzo 0xFF), due
  Steam Controller sullo stesso puck. Niente committato. Installata con il permesso dell'utente
  (`rustybar install`), e `devices` aggiunto alla sua config vera (a destra prima di `network`,
  `pinned = ["mouse", "keyboard", "controller"]`): pill «MX Master 4 25% · Naga 100% in carica ·
  Steam Controller 90%»; le due tastiere in quel momento non rispondevano al ricevitore (dormono:
  la Falcata aveva risposto pochi minuti prima), quindi «Non collegato» e fuori dalla pill finché
  non sono sveglie a un giro. L'utente ha voluto tenerle: «Chi dorme resta» (sopra), con un test
  (`a_keyboard_that_sleeps_stays_dimmed`, 99 in tutto), il mouse addormentato nei dati finti
  (`asleep`) e nei due screenshot; reinstallata: pill «BlackWidow 94% · Naga 100% · Steam
  Controller 90%» (dopo la ripartenza la MX Master 4, che dormiva, non è ancora stata letta).
  Poi il nuovo aspetto (sopra, «Aspetto»), scelto dall'utente da una bozza: test 100 (nomi corti,
  i dettagli di un riquadro), clippy senza avvisi, `devices.png` e `bar-right.png` rifatti (il
  popup è largo 324), la pagina del widget guardata con `settings --shot`; reinstallata: pill
  «Naga 100% · Falcata 52% · Steam 89%», popup coi tre riquadri e i sei non collegati.
  Poi i colori a scala (sopra, «Aspetto»): test 101 (`colors_of_levels`), screenshot rifatti,
  reinstallata. Dopo ogni reinstallazione chi dormiva (MX Master 4, Falcata) torna nella pill solo
  quando risponde a un giro: `last` riparte vuoto.
- 14/11/2026, **pill di `audio` in due parti** (chiesto dall'utente: i nomi come su macOS e il
  volume come riempimento; bozze A/B e due stili mostrate, scelta «B con la tinta»; com'è fatta
  sotto `audio` e in `ui.rs`). Provata coi dati finti (`msg shot bar:right`, il popup ⋯ con un
  margine grande, la compatta con `always_compact` fotografata dallo schermo), con una copia dei
  dati finti a due dispositivi e il microfono muto, e su una barra di prova coi dispositivi veri:
  i nomi tolti da `High Definition Audio Device` e da `2- `. Trovati provando coi nomi veri: i
  monitor si chiamano `1 - <nome> (AMD High Definition Audio Device)` e il vivavoce Bluetooth
  `… Hands-Free HF Audio`: tolti tutti e due. Barra di prova con `audio` e l'orologio, a riposo
  per 60 s: 4,8 MB privati, 7,8 MB committed, 0 ms di CPU, 0,27 risvegli/s, 5 thread, 0 figli;
  primo frame 43 ms. Il riempimento si ridisegna col testo, che si ridisegnava già a ogni cambio
  di volume. Binario 3,44 MB. Test 104, più 3 ignorati (nuovi: nomi, pill in due parti,
  geometria delle parti), clippy senza avvisi. `bar-right.png` rifatto. Non provati: un cambio
  di volume dal vivo (cambierebbe il PC), il tema chiaro, una scala diversa dal 100%. Niente
  committato né installato.
