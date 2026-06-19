# LiberSystem - návrh moderního OS

## Obsah

### 1. Úvod a principy

- [Základní směr projektu](#základní-směr-projektu)
- [Proč tento OS místo Linuxu](#proč-tento-os-místo-linuxu)
- [Jazyková politika](#jazyková-politika)
- [System API model](#system-api-model)

### 2. Kernel

- [Návrh kernelu](#návrh-kernelu)
- [Součásti kernelu](#soucasti-kernelu)
- [Paměťový model](#paměťový-model)
- [Kernel object model](#kernel-object-model)
- [Capability model](#capability-model)
- [IPC model](#ipc-model)
- [Syscall model](#syscall-model)
- [Resource accounting](#resource-accounting)

### 3. Systémové služby a boot

- [Boot flow](#boot-flow)
- [SystemManager](#systemmanager)
- [ServiceManager](#servicemanager)
- [DeviceManager](#devicemanager)
- [PermissionManager](#permissionmanager)
- [ResourceManager](#resourcemanager)
- [Drivery](#drivery)
- [System Graph](#system-graph)

### 4. Úložiště a data

- [Storage model](#storage-model)
- [Native filesystem](#native-filesystem)

### 5. Bezpečnost a aktualizace

- [Bezpečnostní model: aktuální rozhodnutí](#bezpečnostní-model-aktuální-rozhodnutí)
- [Immutable systém a update model](#immutable-systém-a-update-model)

### 6. Rozhraní a aplikační model

- [Aplikační model: nativní ABI + WebAssembly/WASI host](#aplikační-model-nativní-abi--webassemblywasi-host)
- [IDL jazyk](#idl-jazyk)
- [Kompatibilita a POSIX-like vrstva (odloženo)](#kompatibilita-a-posix-like-vrstva-odloženo)

### 7. Roadmapa a závěr

- [MVP návrh](#mvp-návrh)
- [Roadmapa](#roadmapa)
- [Licence](#licence)
- [Otevřené otázky](#otevřené-otázky)
- [Doporučený další krok](#doporučený-další-krok)

---

## 1. Úvod a principy
### Základní směr projektu

Cílem je navrhnout nový moderní operační systém od nuly.

Není to:

- Linux distribuce
- Unix-like systém
- Linux-kompatibilní OS

Nepřebírá historické modely:

- žádný POSIX jako primární kernel API
- žádné `/proc`, `/sys`, `/dev`
- žádné mount pointy
- žádný globální root filesystem
- žádné „everything is a file“ (nemíchá procesy, zařízení, sokety ani nastavení do netypovaných bajtových proudů a textových pseudo-souborů)

Místo toho staví na typovaném objektovém / capability modelu - každý zdroj má jasný typ a explicitní rozhraní.
Toto je vědomá designová filozofie - dělat věci moderněji, objektověji a s lepší typovostí.

Hlavní pilíře, kterými se OS odlišuje:

- **Capability-based bezpečnost**
- **Moderní aplikační host** nad nativním typovaným ABI
- **Paměťová bezpečnost (díky Rustu)**

#### Rozhodnutí

- Nestavět na Linux kernelu.
- Nekopírovat Linuxový model adresářové struktury.
- Nepoužívat `/proc`, `/sys`, `/dev` jako primární systémové API.
- Nepoužívat mount pointy.
- Nepoužívat globální root filesystem jako základní model storage.
- Nepoužívat model „root může všechno“ jako primární bezpečnostní model.
- Nepoužívat `ioctl`-style chaos jako hlavní device API.
- Navrhovat OS jako moderní capability-based systém.

#### Mentální model

```text
Kernel = malý bezpečný rozhodčí
Systémové služby = funkce OS
Drivery = izolované restartovatelné služby
Aplikace = sandboxovatelné komponenty
Storage = explicitní volumes, ne mount pointy
System API = typované objektové/capability API
```

#### Princip vrstvení (stackable, vyměnitelné vrstvy)

Systém je navržen jako **stack vrstev, kde je každá vrstva vyměnitelná, dokud drží svůj kontrakt.** Není to pravidlo jen pro jednu konkrétní volbu - je to univerzální designový princip celého OS.

```text
Každá vrstva komunikuje se sousední přes stabilní, typovaný kontrakt (IPC/IDL).
Implementace vrstvy je vyměnitelná, kontrakt zůstává.
Žádná vrstva nesmí být „zadrátovaná" do ostatních tak, že nejde nahradit.
```

Důsledky:

- **Závislost je na kontraktu, ne na konkrétní implementaci.** Vrstva nad ní nemá vědět, *kdo* ji obsluhuje, jen *jaké rozhraní* dostává.
- **Rizikové nebo mladé technologie jdou izolovat za adaptér.** Když se taková technologie změní nebo nahradí, přepíše se jen adaptér té jedné vrstvy, ne systém.
- **Více implementací téhož kontraktu může koexistovat** (např. víc filesystémových backendů za jedním Volume API, víc aplikačních hostů nad stejným nativním IPC).

Tento princip se promítá konkrétně do aplikačního modelu (viz *Aplikační model*: WASI je jen jeden z hostů nad stabilním nativním ABI), storage (víc FS backendů za Volume API) i API vrstvy (víc reprezentací nad jedním typovaným modelem).

---

### Proč tento OS místo Linuxu

Krátká, poctivá odpověď: protože nabízí model, který se do Linuxu kvůli 30 letům zpětné kompatibility už nedá doplnit. Hlavní důvody (seřazené podle síly):

1. **Capability-based bezpečnost jako základ.** Žádné „root může všechno", žádná ambient authority. Princip nejmenšího oprávnění je strukturální - mizí celé třídy chyb (privilege escalation, confused deputy).
2. **WebAssembly/WASI jako nativní aplikační model.** Přenositelné, sandboxované, jazykově neutrální aplikace, jeden artefakt běží na x86/ARM/RISC-V. Bezpečnost a přenositelnost „zadarmo".
3. **Paměťová bezpečnost od základu (Rust).** Eliminuje většinu tříd CVE, které trápí jádra v C (memory-safety bugy).

Podpůrné důvody:

- **Spolehlivost microkernelu** - pád ovladače/služby nepoloží systém, jen se restartuje.
- **Čisté, typované, prozkoumatelné API** - jedno API se čtyřmi reprezentacemi (binary/CBOR/JSON/CLI), žádné scrapování textu z `/proc`, žádný `ioctl` chaos. Skvělé pro nástroje a automatizaci.
- **Explicitní storage model** - žádný tichý zápis na špatný disk, žádná překvapení z mount pointů.
- **Bez legacy dluhu** - moderní volby bez nutnosti táhnout dekády kompatibility.
- **Open source** - maximální volnost použití i forků.

#### Pro koho je systém určen v současné fázi

V současnosti je systém **určen vývojářům, early-adopterům a nasazení v oblasti edge/security a výzkumu**, nikoli jako náhrada Linuxu v krátkodobém horizontu:

- Je to záměr, nikoliv slabina - představuje **vstupní bod dlouhodobého směřování**, nikoli konečný stav.
- Ranou cílovou skupinou jsou ti, které zaujme capability model, WASI a čistá architektura a kteří začnou budovat ekosystém aplikací, nástrojů a ovladačů.
- Hnacím prvkem směřování není „modernost a absence legacy" sama o sobě, ale **capability bezpečnost, aplikační model WASI a paměťová bezpečnost**.

Další směřování systému (server → reálný hardware → desktop → AI platforma) i okamžik otevření širšímu publiku popisuje *Roadmapa*.

#### Nasazovací cíle: appliance/edge → server → desktop

Kromě vrstvení *v čase* (kdo) má projekt i jasné pořadí *nasazovacích cílů* (kde). Cíle jsou seřazeny podle jednoho principu: **od maximální vlastní kontroly k minimální**. Čím více systém kontroluje vlastní hardware i software, tím menší externí ekosystém potřebuje - a právě závislost na externím ekosystému bývá nejčastější příčinou neúspěchu nových operačních systémů.

| Cíl | Pořadí | Kdo řídí hardware | Kdo píše běžící software | Potřebný externí ekosystém |
|---|---|---|---|---|
| **Appliance / edge / embedded** | **1. (současná)** | projekt (jeden board / VM profil) | projekt (nativní / WASI) | minimální |
| **Server** | 2. | částečně projekt | částečně externí (služby, DB) | střední (POSIX kompat.) |
| **Desktop** | 3. | kdokoli (neomezené HW) | celý svět (GUI aplikace) | rozsáhlý (zde selhává většina OS) |

Klíčová pravidla tohoto pořadí:

- **Každý cíl je nadmnožinou předchozího.** Z embedded na server přibývá především síť a POSIX kompatibilita (kvůli externímu softwaru), ze serveru na desktop pak GUI, vstup, zvuk a širší škála ovladačů. Nejde o tři nezávislé starty od nuly, ale o budování ve vrstvách (viz *Princip vrstvení*).
- **Každý cíl je samostatně hodnotný, nikoli pouze odrazový můstek.** Již appliance/edge představuje plnohodnotný produkt sám o sobě (bezpečný edge uzel), takže systém dodává reálnou hodnotu od první fáze - nikoli až na konci cesty. Server i desktop na tomto základu staví jako plnohodnotná rozšíření, k nimž systém směřuje.

### Jazyková politika

OS je psaný primárně v Rustu.

#### Rozhodnutí

```text
Safe Rust prakticky všude.
Unsafe Rust jen tam, kde je to opravdu nutné.
Assembler jen tam, kde je nevyhnutelný.
C/C++ nepoužívat pro nový core OS kód.
C/C++ připustit jen výjimečně pro převzaté knihovny, dočasné porty nebo velké externí stacky.
```

#### Kde je safe Rust

- většina kernel logiky,
- scheduler logika,
- IPC,
- capabilities,
- handle tables,
- object lifecycle,
- resource accounting,
- SystemManager,
- ServiceManager,
- DeviceManager,
- StorageService,
- Log/EventService,
- System Graph,
- CLI nástroje,
- nativní filesystém,
- většina userspace driverů.

#### Kde je potřeba `unsafe Rust`

- page tables,
- MMIO,
- DMA,
- IOMMU,
- raw fyzická paměť,
- interrupt setup,
- CPU registry,
- přechod kernel/userspace,
- nízkoúrovňové arch-specific operace.

#### Kde je assembler

- boot glue,
- context switch,
- syscall entry/exit,
- interrupt entry/exit,
- případně velmi nízkoúrovňové CPU operace.

#### Pravidlo pro unsafe

```text
Unsafe je karanténa pro kontakt s hardwarem.
Unsafe nesmí být běžný styl programování.
Unsafe musí být malé, auditovatelné a obalené safe API.
```

---

### System API model

Systém nepoužívá `/proc`, `/sys`, `/dev` ani textové pseudo-soubory jako hlavní API.

#### Rozhodnutí

Existuje jedno kanonické typované API.

Nad ním jsou různé reprezentace:

| Reprezentace | Účel |
|---|---|
| Binary / IDL | rychlá komunikace mezi systémovými částmi |
| CBOR | kompaktní strukturovaná data včetně byte stringů |
| JSON | skripty, debug, vzdálená administrace |
| Human CLI | čitelný výstup pro člověka |

#### Důležité pravidlo

```text
Neexistují 4 různá API.
Existuje jedno typované API a 4 reprezentace stejných dat.
```

CLI, JSON, CBOR a binary výstup jsou jen různé pohledy na stejný objektový model.

#### Princip: objekt je kanon (platí v celém systému)

Toto není jen pravidlo o systémovém API - je to **systémový princip** platný všude, kde se v systému pojmenovává, přenáší nebo ukládá strukturovaná informace:

> Kanonem je vždy **typovaný objekt** definovaný v IDL.
> Text / URI / JSON / CBOR / CLI / ... jsou jeho **reprezentace**.
> Identita a autorita jsou v **capability**, ne ve jméně.

Pro cesty je princip rozepsaný v sekci *Storage model* („Cesta je objekt, URI je jen reprezentace“), tady se zobecňuje na zbytek systému.

##### Kde se princip uplatňuje

| Oblast | Kanonický objekt (návrh) | Reprezentace | Autorita / poznámka |
|---|---|---|---|
| **Konfigurace** (`ConfigService`) | typovaný strom `ConfigNode` se schématem v IDL | JSON / CBOR / CLI / binárka | žádné parsování textových `/etc` souborů, text je jen editovatelná reprezentace |
| **Logy** (`LogService`) | `LogRecord { ts, severity, source, fields }` | human text, JSON, CBOR, binárka | log jsou dotazovatelná strukturovaná data, ne řádky textu (model journald, ne syslog) |
| **Identita služby / driveru** | `ServiceId` / `ComponentId` | string `driver.usb`, JSON | autorita je vždy **handle/capability na Channel**, ne „najdi službu podle jména“ |
| **Chyby / stavy** | typovaný `Error` (variant) | číselný ABI kód, lidský string, JSON | žádné errno-style holé inty ani ad-hoc textové hlášky |
| **Síťové adresy / endpointy** (`NetworkService`) | `Endpoint` / `SocketAddr` / `{ ip, port }` | `192.168.0.1:80`, URL text | parsování stringu je zdroj děr, typ je bezpečný (analogie k `VolumePath`) |
| **Nástěnný čas** (sekce *Syscall model*) | `Timestamp` / `Instant` | ISO-8601, epoch, lidský formát | monotonní čas je int (ns), kalendářní čas je objekt |
| **Manifest balíčku / oprávnění** (sekce *Bezpečnostní model*) | typovaný `Manifest` / `PermissionSet` | text pro ruční editaci, JSON | model je objekt, ne YAML/JSON soubor jako „zdroj pravdy“ |
| **System Graph** (sekce *System Graph*) | graf typovaných odkazů na objekty | strom/obrázek, JSON, CBOR, CLI | uzly = odkazy na Process / Service / Driver / Device / Volume |

##### Kde princip naopak NEcpát

- **Hromadná binární data** - obsah souboru, DMA / shared-memory payload, video frame, síťový paket. To jsou záměrně neprůhledné bajty, princip je o strukturovaných *identifikátorech a metadatech*, ne o obalování každého bufferu schématem.
- **Výkonové horké cesty** - mohou mít jen jednu kanonickou binární formu a ostatní reprezentace negenerovat. Reprezentace je *možnost*, ne povinnost u každé hodnoty.

#### Příklady služeb

```text
ProcessService
StorageService
DeviceService
NetworkService
GraphicsService
AudioService
ConfigService
LogService
SystemGraphService
```

#### Co nechceme

```text
cat /proc/meminfo
cat /sys/class/net/...
open("/dev/input/event0")
ioctl(fd, MAGIC, ptr)
```

#### Co chceme

```text
MemoryService.GetInfo()
DeviceService.List()
InputService.Subscribe(...)
Storage.Volume.Open(...)
```

A k tomu:

```text
command
command --json
command --cbor
command --binary
```

---

## 2. Kernel

### Návrh kernelu

Kernel je malé capability-based message jádro.

Kernel není „celý operační systém“. Kernel je pouze bezpečnostní, plánovací a izolační základ.

#### Kernel ví

- kdo běží,
- jakou má paměť,
- s kým smí komunikovat,
- jaké capabilities drží,
- k jakému hardwaru má přístup,
- co je potřeba uklidit při pádu.

#### Kernel neví

- co je soubor v uživatelském smyslu,
- co je volume alias,
- co je okno,
- co je audio stream,
- co je síťové připojení,
- co je aplikace v produktovém smyslu,
- co je package manager,
- co je update systému.

---

### Součásti kernelu

**V kernelu je:**

Do kernelu patří jen to, co musí být privilegované, absolutně důvěryhodné nebo přímo vynucuje izolaci.

| Oblast | Je v kernelu | Důvod |
|---|---:|---|
| Boot převzetí řízení | ano | kernel musí nastartovat |
| CPU management | ano | řízení CPU, režimů, jader |
| Scheduler | ano | rozhoduje, co běží |
| Thread low-level model | ano | základ běhu kódu |
| Process low-level model | ano | izolovaný kontejner běhu |
| Address spaces | ano | izolace paměti |
| Fyzická RAM | ano | vlastnictví a alokace stránek |
| Virtuální paměť | ano | page tables, mapování, ochrana |
| Memory protection | ano | read/write/execute, guard pages |
| IPC / message passing | ano | základ komunikace služeb |
| Capabilities / handles | ano | bezpečnostní model |
| Kernel object model | ano | základní primitiva systému |
| Interrupt routing | ano | bezpečné doručování IRQ |
| Timers | ano | scheduler, timeouty, sleep |
| IOMMU / DMA ochrana | ano | zařízení nesmí zapisovat kamkoliv |
| MMIO mapping | ano, kontrolovaně | driver dostane jen registry svého zařízení |
| Device access control | ano | vynucení práv k HW |
| Shared memory primitives | ano | výkonné sdílení dat |
| Event/wait primitives | ano | čekání bez pollingu |
| Fault detection | ano | page fault, illegal instruction, crash |
| Resource cleanup | ano | odebrání paměti, IRQ, DMA, capabilities |
| Resource accounting primitives | ano | limity RAM, handles, IPC fronty, DMA |
| Start prvního userspace procesu | ano | spuštění SystemManageru |
| Early logging / panic | ano | nouzová diagnostika |
| Recovery základ | minimálně | pokud spadne SystemManager |

#### Nejkratší definice kernelu

```text
Kernel = paměť + plánování + IPC + capabilities + bezpečný hardware access + cleanup.
```

#### SMP / multicore: návrhový constraint od začátku

Multicore není feature, která se „přišroubuje později". Neznamená to, že se v MVP musí hned ladit na výkon - ale **datové struktury, zámky a IPC se od první verze navrhují jako SMP-aware**, i když se SMP zatím neoptimalizuje (klidně se bootuje a běží na jednom jádře).

```text
PRAVIDLO:
Kernel se navrhuje SMP-aware od Fáze 0.
SMP se v MVP nemusí optimalizovat (klidně běh na jednom jádře),
ale žádná datová struktura ani invariant nesmí předpokládat single-core.
```

Proč to musí být od začátku, a ne až později:

- **SMP prosakuje do celého zamykacího a IPC modelu.** Per-CPU run queue, jak se předávají handly mezi jádry, jak se účtují zdroje napříč CPU, jak se synchronizuje handle table a capability operace - to vše rozhoduje, jestli je jádro SMP-ready. Tyhle volby nejdou udělat „až potom" bez přepisu základů.
- **Retrofit SMP do single-core jádra je drahý a bolavý.** Jádro navržené na jedno jádro má typicky jeden velký zámek, rozbít ho později na jemné zamykání je přesně ta cesta, kterou Linux platil roky (Big Kernel Lock a jeho postupné odstraňování). Tomu se vyhneme tím, že granularitu zámků navrhneme správně hned.
- **Capability a Domain model na to musí myslet.** Předání, duplikace i revoke handlu a účtování do `Domain` se může dít z více jader současně. Když je to SMP-aware od začátku, je to jen otázka správného zámku/atomiky, když ne, je to pozdější přepis bezpečnostně kritického kódu.

Pro MVP stačí běh na jednom jádře, ale **návrh musí být multicore-ready od Fáze 0**:

- Vlastní SMP scheduling, per-CPU run queue a load balancing se ladí později.
- Návrhový základ (granularita zámků, SMP-aware datové struktury) ale musí stát hned.

---

**V kernelu není:**

| Oblast | Kde je |
|---|---|
| filesystémy | filesystem driver služba |
| StorageService / volumes | userspace služba |
| USB stack | `driver.usb` |
| NVMe/SATA/AHCI | driver služby |
| GPU driver | `driver.gpu` + `GraphicsService` |
| audio stack | `AudioService` |
| síťový stack | `NetworkService` |
| Wi-Fi/Bluetooth | driver služby |
| grafický compositor | `GraphicsService` |
| input routing | `InputService` |
| config systém | `ConfigService` |
| hlavní logovací systém | `LogService` |
| package manager | pozdější `PackageManager` |
| update systém | pozdější `UpdateService` |
| restart driverů | `ServiceManager` + `DeviceManager` |
| app sandbox policy | vyšší vrstva / později |
| JSON/CBOR/human CLI rendering | API/CLI vrstva |
| `system://`, `user://`, `vol://` resolver | userspace služby |
| `/proc`, `/sys`, `/dev` | vůbec nedělat |

---

### Paměťový model

Kernel spravuje:

```text
fyzickou RAM
virtuální paměť
page tables
sdílené buffery
DMA buffery
ochranu mapování
```

Kernel nespravuje jako hlavní vlastník:

```text
VRAM
GPU textures/surfaces
filesystem cache
heap aplikací
```

Rozdělení:

```text
Kernel Memory Manager:
  RAM, mapování, izolace, DMA safety

GPU/GraphicsService:
  VRAM, textures, surfaces, framebuffers

StorageService:
  file cache, block cache

Aplikace/runtime:
  heap allocator
```

---

### Kernel object model

Kernel má znát malé množství objektů.

Navržené kernel objekty:

| Objekt | Význam |
|---|---|
| `Domain` | hierarchický kontejner procesů (skupina pro limity, recovery a hromadné ukončení) |
| `Process` | izolovaný běžící kontejner |
| `Thread` | konkrétní běžící vlákno |
| `AddressSpace` | virtuální paměť procesu |
| `MemoryObject` | kus RAM nebo sdílené paměti |
| `Channel` | komunikační kanál |
| `Event` | čekání/probuzení/signál |
| `Timer` | časovač |
| `Interrupt` | přerušení předané driveru |
| `DeviceMemory` | povolená MMIO oblast |
| `DmaBuffer` | DMA-safe paměť |
| `Capability` | právo k objektu |
| `Handle` | držení capability procesem |

#### Hierarchie: Domain

Procesy nejsou ploché - tvoří strom pod uzly typu `Domain` (obdoba Zircon *Job*). `Domain` je skupinový uzel, na který se věší:

- **resource limity** pro celou podskupinu (paměť, počty handle/threadů, IPC fronty, DMA) - viz Resource accounting,
- **hromadné ukončení**: zabití `Domain` ukončí celý podstrom (proces + jeho potomky) a kernel uklidí všechny jejich handly,
- **recovery a izolace**: kritické subsystémy (např. všechny drivery) mohou běžet ve vlastní `Domain`, takže jdou restartovat jako celek.

Strom typicky vypadá:

```text
root Domain
├── SystemManager
│   ├── ServiceManager
│   │   ├── LogService
│   │   └── StorageService
│   └── DeviceManager Domain
│       ├── driver.virtio-blk
│       └── driver.virtio-net
└── Apps Domain
    ├── app A (WASM komponenta)
    └── app B (WASM komponenta)
```

Tento model je nutné ještě dopracovat do detailní specifikace, ale směr je rozhodnutý.

---

### Capability model

Kernel nepoužívá primární model:

```text
root / user / group
```

Místo toho:

```text
capability = nepodvrhnutelné právo k objektu
handle = konkrétní capability v tabulce procesu
```

#### Pravidla

- Capability nejde uhodnout.
- Capability nejde vyrobit z čísla.
- Capability jde získat pouze předáním.
- Capability může být omezená právy.
- Capability může být předána přes IPC.
- Při pádu procesu kernel smaže jeho handle table.
- Tím proces ztratí přístup ke všem objektům.

#### Příklad práv

```text
read
write
execute
map
send
receive
duplicate
transfer
revoke
```

#### Příklad handle table

Proces `driver.nvme` může mít:

```text
handle 1 -> Channel to DeviceManager
handle 2 -> PCI device capability
handle 3 -> MMIO region BAR0
handle 4 -> Interrupt 42
handle 5 -> DMA domain
handle 6 -> LogService channel
```

Při pádu driveru kernel automaticky odebere všechny handly a související oprávnění.

#### Jak to má fungovat správně (detailní model)

**Capability = (odkaz na kernel objekt + sada práv + badge), držená výhradně v kernelu.** Userspace nikdy nedrží surovou capability, drží jen *handle* - neprůhledný index do své handle table. To je stejný princip jako file descriptor, ale pro všechny objekty systému.

```text
Capability {
  object:  ref na kernel objekt (Process, Channel, MemoryObject, …)
  rights:  bitová sada povolených operací
  badge:   volitelné neměnné označení nastavené při vzniku
}
Handle = index do per-proces handle table -> Capability
```

**Práva (rights).** Operace nad objektem je povolená jen tehdy, když ji handle nese. Práva se nedají z handlu „dovymyslet“.

```text
read         write        execute
map          send         receive
duplicate    transfer     revoke
get_info     manage       wait
```

**Atenuace (zužování práv).** Z capability lze odvodit *slabší*, nikdy silnější. `handle_duplicate` umí vytvořit kopii jen s podmnožinou původních práv. Tím se přirozeně vynucuje princip nejmenšího oprávnění při předávání.

```text
handle(read|write|duplicate)  --duplicate(read)-->  handle(read)
```

**Badging (rozlišení klientů na sdíleném kanálu).** Server může jednomu Channelu přidělit více klientů, každému s jiným neměnným `badge`. Kernel badge připojuje ke zprávě, takže server bezpečně pozná „od koho to je“ bez možnosti podvržení. Slouží jako základ identit a per-klient politik.

**Předání (transfer).** Capability se získá *jedině* předáním po Channelu (`handle_transfer`). Nelze ji uhodnout, vyrobit z čísla ani „najít“ v globálním namespace. Předání může být:

- *move* (odesílatel handle ztrácí), nebo
- *copy* s atenuací (odesílateli zůstává, příjemce dostane slabší).

**Revokace.** Odebrání práva má dvě úrovně:

- *zavření handlu* (`handle_close`) - lokální, jen daný proces ztrácí přístup,
- *revoke objektu* - kernel zneplatní *všechny* handly na objekt (např. StorageService odvolá přístup k volume, které mizí). Implementačně přes generační čítač / revocation u objektu, aby revokace byla O(1) a nešla obejít.

**Sealing / typovost.** Capability je vždy svázaná s typem objektu, nelze poslat „MemoryObject tam, kde se čeká Channel“. Typovou kontrolu dělá kernel, ne klient.

**Lifecycle a úklid.** Objekty jsou reference-counted, žijí, dokud na ně existuje handle (nebo je drží kernel/zpráva na cestě). Při pádu procesu kernel projde jeho handle table, každý handle zavře (sníží refcount) a tím se uvolní i navázané zdroje (IRQ, DMA, MMIO). To je celý „bezpečnostní úklid“ - nevyžaduje spolupráci spadlého procesu.

**Co tím získáváme.**

- Žádná ambient authority - proces má přesně to, co dostal předané, nic víc.
- Mizí *confused deputy* i *privilege escalation přes „root“* - žádné globální „root může všechno“.
- Bezpečnost je strukturální vlastnost grafu předaných capabilit, ne sada kontrol roztroušených v kódu.

---

### IPC model

Základní kernel objekt pro komunikaci je `Channel`.

#### Malé zprávy

Používají se pro běžné řízení:

```text
Storage.Open(path, rights)
Device.GetInfo()
Process.Start()
Log.Write()
```

#### Velká data

Velká data se neposílají přímo v IPC zprávě.

Místo toho:

```text
message = metadata + handle na SharedBuffer / DmaBuffer
```

To je důležité pro:

- storage,
- síť,
- audio,
- video,
- GPU,
- vysokovýkonné IPC.

#### Model komunikace (rozhodnuto)

Páteř je **asynchronní a neblokující na úrovni kernelu**, nad ním stojí pohodlné synchronně vypadající klientské API. Přesně tak to dělá Zircon a osvědčilo se to.

```text
Kernel primitiva:  channel_send / channel_receive jsou NEblokující.
Čekání:            jediné místo, kde se blokuje, je `wait`
                   (čeká na čitelnost/zapsatelnost kanálu, event, timer).
Klientské API:     nad tím generuje IDL „call(req) -> resp“,
                   která interně udělá send + wait + receive.
```

Důvody:

- Neblokující jádro = žádné držení zámků kernelu během čekání, lepší škálování a odolnost.
- Server si sám řídí, kdy a kolik zpráv přijme (přirozený **backpressure**).
- Async streamy i request/response jdou postavit nad stejným primitivem.

**Request/response** je konvence nad Channelem: zpráva nese `correlation id`, odpověď se posílá zpět na reply-handle. Synchronní `call()` je jen `send` + `wait` + `receive` zabalené generovaným kódem.

**Event streamy** = jednosměrný Channel, kde server publikuje a klient čte přes `wait` (žádný polling).

**Backpressure**: každý Channel má omezenou frontu (účtovanou do resource accountingu odesílatele). Plná fronta → `send` vrátí `WOULD_BLOCK`, odesílatel počká přes `wait`. Zpráva se nikdy nezahodí potichu.

**Timeouts**: řeší `wait` s deadlinem (přes `Timer`), ne samostatný mechanismus v každém volání.

##### Default wire formát: decode-cheap binárka (rozhodnuto)

Default drátový formát IPC zpráv je **levná-na-dekódování binárka ve stylu FIDL / Cap'n Proto**, ne CBOR ani JSON. Je to vědomé rozhodnutí: **microkernel stojí a padá na ceně IPC** a každý hop typovaný record (de)serializuje, takže default formát musí být levný na čtení.

```text
PRAVIDLO:
Default IPC wire = decode-cheap binární layout (fixní offsety / in-place čtení, FIDL/Cap'n Proto styl).
CBOR/JSON NEJSOU default přenosový formát - jsou to volitelné reprezentace
pro skripty, debug a vzdálenou administraci (viz System API model).
```

Důvody:

- **Dekódování bez parsování.** Fixní offsety / in-place čtení znamenají, že příjemce sáhá na pole přímo v bufferu bez alokace a bez plného parseru - řádově levnější než CBOR/JSON.
- **Nízká latence na horké cestě.** Storage, grafika a další horké cesty si nemůžou dovolit cenu strukturovaného parseru na každý `call()`.
- **„Objekt je kanon" tím neporušujeme.** Kanonem zůstává typovaný objekt z IDL, decode-cheap binárka je jeho **default reprezentace na drátě**, CBOR/JSON/CLI jsou ostatní (volitelné) reprezentace (viz *System API model*). U horkých cest se ostatní reprezentace prostě negenerují.

Rozlišení, aby nedošlo k záměně: tohle je o **formátu řídicích zpráv (control-plane)**. Velká data se stejně neposílají v těle zprávy, ale jako `handle na SharedBuffer / DmaBuffer` (zero-copy, viz výše) - default wire formát se jich netýká.

##### Zbývá doladit

- přesný binární message layout (*styl* je rozhodnutý - decode-cheap, zbývají konkrétní bajty, vyřeší IDL/WIT),
- priority front a fair-scheduling zpráv,
- konkrétní default velikosti front.

##### Ověřit měřením co nejdřív

Microkernel stojí a padá na výkonu IPC - historicky na něm umřela řada systémů. Proto platí: **jakmile poběží Channel IPC (Fáze 0), hned měřit, ne odhadovat.**

- **Round-trip latence lokálního `call()`** (send + wait + receive) - **měří se hned, jak poběží Channel (Fáze 0), a je to brána, ne hezké přání**: dokud round-trip nesedí do cílového rozpočtu (řádově jednotky µs), nestaví se na IPC vyšší vrstvy. Měřit průběžně, ne až nakonec.
- **Zero-copy pro velká data** - empiricky potvrdit, že „metadata + handle na shared/DMA buffer" reálně nekopíruje payload. Tohle je v návrhu správně, ale musí se to ověřit, ne předpokládat.
- **Cena typované serializace na hranici** - kde je „objekt je kanon" levný a kde se u horkých cest (storage, grafika) vyplatí jen jedna binární forma.

Smysl měření je **potvrdit nebo vyvrátit**, kde je daný návrh dobrý a kde se musí udělat jinak - dřív, než se na něm postaví vyšší vrstvy.

---

### Syscall model

Kernel má mít málo syscallů.

Navržená minimální sada:

| Syscall | Význam |
|---|---|
| `process_create` | vytvořit proces |
| `process_exit` | ukončit proces |
| `thread_create` | vytvořit thread |
| `thread_start` | spustit thread |
| `channel_create` | vytvořit kanál |
| `channel_send` | poslat zprávu |
| `channel_receive` | přijmout zprávu |
| `wait` | čekat na event/channel/timer |
| `memory_object_create` | vytvořit paměťový objekt |
| `memory_map` | namapovat paměť |
| `memory_unmap` | odmapovat paměť |
| `handle_duplicate` | zkopírovat handle s omezenými právy |
| `handle_transfer` | předat handle |
| `handle_close` | zavřít handle |
| `timer_create` | vytvořit timer |
| `interrupt_bind` | předat interrupt driveru |
| `device_memory_map` | povolit MMIO oblast |
| `dma_buffer_create` | vytvořit DMA-safe buffer |
| `fault_info_get` | informace o pádu procesu |
| `domain_create` | vytvořit `Domain` (skupinu procesů) |
| `domain_kill` | ukončit `Domain` i s celým podstromem |
| `object_info_get` | introspekce objektu (typovaně) |
| `object_property_set` | nastavit vlastnost objektu (název, limit…) |
| `event_signal` | nastavit/shodit signál na `Event` |
| `clock_get` | přečíst monotonní čas kernelu (běh od bootu) |
| `random_get` | kryptografická náhoda z kernelu |

Kernel syscall nemá být:

```text
file_open
file_read
socket_create
window_draw
audio_play
device_list
```

To jsou funkce služeb, ne kernelu.

#### Realistické očekávání rozsahu

„Málo syscallů“ je princip, ne tvrdé číslo. Výše uvedená sada je *jádro*, reálně poroste směrem k ~50-100 voláním (introspekce, ladění, správa `Domain`, vlastnosti objektů, čas, náhoda). Důležitější než počet je:

- **stabilní, verzované ABI** - syscall rozhraní se nesmí měnit nekompatibilně, nová volání se přidávají, stará nemizí,
- **úzká sémantika** - každý syscall dělá jednu věc nad kernel objektem,
- **žádné „service“ operace v kernelu** - vše ostatní jde přes IPC na služby.

Pro srovnání: Zircon cílí na ~100 syscallů a je to stále „malý“ kernel.

#### Čas: co vrací `clock_get`

`clock_get` vrací **monotonní čas kernelu** - počítadlo navázané na hardwarový timer, které startuje (v podstatě od nuly) při bootu, jen narůstá a nikdy nejde zpět. **Není to kalendářní datum a čas**, ale plynoucí základ pro:

- timeouty a deadliny (`wait`),
- měření trvání („kolik uběhlo“),
- scheduler.

Jednotka je v ABI pevně daná (nanosekundy), sekundy/milisekundy si volající dopočítá. Protože na monotonním čase závisí scheduler i timeouty, je **z principu nenastavitelný** - kdyby šlo „přetočit“, rozbila by se veškerá logika závislá na čase.

**Nástěnný čas (reálné datum a čas, UTC) není stav kernelu, ale policy** userspace služby (`TimeService`, případně část `ConfigService`):

- TimeService drží offset a počítá `UTC = clock_get (monotonic) + offset` (+ časová zóna, DST… čistě userspace věc).
- Offset se získává z RTC (přes ovladač) nebo z NTP.
- **Nastavení reálného času = capability-gated operace služby**, ne syscall - smí ji jen držitel handle na TimeService s právem `write` (NTP klient, ovladač RTC, admin nástroj). Žádné globální „root nastaví čas“.
- Rychlé čtení UTC může jít přes read-only sdílené mapování offsetu (vDSO-style), aby nešlo o IPC při každém dotazu.
- Nástěnný čas se v API předává jako typovaný objekt `Timestamp` (kanon), ISO-8601, epoch i lidský formát jsou jen jeho reprezentace (viz *System API model*).

---

### Resource accounting

Kernel má od začátku počítat a vynucovat základní zdroje.

Ne jako plný Linux cgroups model, ale jednodušeji.

Každý proces má resource account:

```text
memory_used
handle_count
thread_count
ipc_queue_bytes
dma_bytes
```

Kernel musí umět odmítnout:

```text
další DMA buffer
další thread
další handle
další IPC zprávu při plné frontě
další memory object při překročení limitu
```

Policy může později řešit `ResourceManager`, ale vynucení musí být v kernelu.

#### Jak to má fungovat správně

**Účet má proces i `Domain`.** Limity se skládají hierarchicky: proces nesmí překročit svůj limit ani součtový limit své `Domain`. Tím jde dát rozpočet celé skupině (např. „všechny aplikace dohromady max N MB“).

**Kdo platí za zprávu „na cestě“.** Jasné pravidlo proti DoS: **paměť zprávy ve frontě se účtuje odesílateli, dokud ji příjemce nepřevezme.** Plná fronta příjemce → `send` vrátí `WOULD_BLOCK` (zpráva se nezahodí, odesílatel počká). Tím nemůže odesílatel zahltit příjemce ani si „zadarmo“ alokovat paměť v cizím účtu.

**Vynucení na hranici, ne uprostřed.** Kontrola limitu je atomická součást operace, která zdroj vytváří (`*_create`, `memory_map`, `channel_send`). Buď operace projde a zdroj je započítán, nebo selže - nikdy ne „napůl alokováno“.

**Účtuje se skutečný zdroj, ne abstrakce.** Minimálně:

```text
memory_used     fyzické stránky držené procesem (vč. sdílených podle podílu)
handle_count    počet handle v tabulce
thread_count    počet threadů
ipc_queue_bytes paměť zpráv čekajících ve frontách (na straně odesílatele)
dma_bytes       pinned DMA paměť
```

**Selání je první-třídní stav, ne panika.** Při překročení limitu kernel vrací typovanou chybu (`RESOURCE_EXHAUSTED`), službám se tím dává šance reagovat (zpomalit, uvolnit cache), místo aby spadly.

**Úklid vrací zdroje okamžitě.** Zánik procesu/`Domain` strhne refcounty → paměť, handly, DMA i místo ve frontách se uvolní bez spolupráce spadlé komponenty.

Pro MVP stačí počítat a vynucovat `memory_used`, `handle_count`, `thread_count`, fronty a DMA přibydou s IPC a drivery.

---

## 3. Systémové služby a boot

Tyto vrstvy nejsou ještě detailně specifikované, ale základní odpovědnosti jsou jasné.

### Boot flow

Navržený start systému:

```text
1. Bootloader načte kernel + init package.
2. Kernel inicializuje CPU, paměť, interrupts, timer.
3. Kernel vytvoří první AddressSpace.
4. Kernel spustí první userspace proces: SystemManager.
5. SystemManager spustí ServiceManager.
6. ServiceManager spustí DeviceManager, LogService, StorageService.
7. DeviceManager začne spouštět drivery.
8. StorageService zpřístupní první volume.
9. CLI nebo GUI se spustí jako běžná komponenta.
```

#### Volba bootloaderu (rozhodnuto)

**Vlastní bootloader se nepíše** - je to týdny práce bez přidané hodnoty. Pro MVP se použije hotový, moderní bootloader:

- **Limine** jako primární volba (čistý, dělaný přímo pro nové/hobby OS, podporuje x86-64 i ARM64, předává paměťovou mapu, framebuffer, moduly).
- **přímý UEFI** jako alternativa, pokud je potřeba větší kontrola.

Vlastní boot kód se omezí na nutné *boot glue* (převzetí řízení od bootloaderu, přechod do vlastního prostředí). Volba bootloaderu neovlivňuje architekturu kernelu - je to vyměnitelná vstupní brána.

#### První praktický cíl

```text
kernel
SystemManager
LogService
StorageService nad ramdiskem
CLI shell
```

---

### SystemManager

- první userspace proces,
- start základních systémových služeb,
- recovery při pádu některých kritických částí,
- předání řízení vyšším službám.

#### Recovery při pádu SystemManageru

Pokud spadne, kernel má umět minimální recovery chování:

```text
1. spustit recovery SystemManager,
2. spustit emergency shell,
3. bezpečně restartovat userspace,
4. reboot,
5. panic, pokud není jiná možnost.
```

Toto je jediná výjimka, kde kernel má mít minimální záchranný mechanismus nad rámec čistého mechanismu.

### ServiceManager

- spouštění služeb,
- zastavování služeb,
- restart policy,
- dependency management,
- heartbeat/watchdog,
- evidence stavu služeb.

### DeviceManager

- detekce zařízení,
- mapování zařízení na drivery,
- přidělování device capabilities driverům,
- stav zařízení,
- reakce na pád driveru.

### PermissionManager

- policy přidělování capabilities,
- později detailní app sandbox.

Detailní bezpečnostní politika a její fázování (co platí od MVP, co se odkládá) je v sekci *Bezpečnostní model: aktuální rozhodnutí*.

### ResourceManager

- policy pro limity zdrojů,
- quotas,
- možná později CPU/GPU/network/storage budgets.

---

### Drivery

Drivery jsou mimo kernel jako izolované služby.

#### MVP: jen virtio na QEMU/KVM

Aby projekt nezamrzl na ovladačích reálného (a buggy) hardwaru, **první cíl je výhradně virtio na QEMU/KVM.** Virtio je čisté, dobře dokumentované a stačí na plnohodnotný systém ve virtuálu:

```text
driver.virtio-blk      # blokové úložiště
driver.virtio-net      # síť
driver.virtio-console  # sériová konzole / log
driver.virtio-gpu      # framebuffer / 2D, později akcelerace
driver.virtio-input    # klávesnice / myš
```

Reálný HW (USB, NVMe, AHCI, GPU, Wi-Fi, audio) se přidává **postupně a podle potřeby** - když to někdo chce nasadit na konkrétní stroj. Vlastní GPU/Wi-Fi stack se v dohledné době záměrně nepíše (je to nejčastější hřbitov nových OS).

#### Cílové ovladače (později)

```text
driver.usb
driver.nvme
driver.gpu
driver.audio
driver.network
driver.fs.modernfs
driver.fs.ext4
driver.fs.ntfs
```

#### Pád driveru

Pokud spadne například `driver.usb`:

```text
1. Driver udělá fault.
2. Kernel zastaví proces driveru.
3. Kernel odebere jeho capabilities.
4. Kernel odpojí jeho IRQ.
5. Kernel zakáže jeho DMA přístup.
6. Kernel uvolní jeho paměť.
7. Kernel pošle event ServiceManageru.
8. DeviceManager označí zařízení jako offline/restarting.
9. ServiceManager podle policy driver restartuje.
10. Driver znovu inicializuje zařízení.
```

Kernel neřeší, zda se USB má restartovat. Kernel jen bezpečně uklidí škody a pošle event.

Restart policy patří do `ServiceManager` / `DeviceManager`.

---

### System Graph

System Graph je schválený koncept.

V souladu s principem *objekt je kanon* (sekce *System API model*) je System Graph **graf typovaných odkazů na objekty** - uzly jsou Process / Service / Driver / Device / Volume, hrany jsou kanály a závislosti. Vizuální strom je jen jedna reprezentace, graf je stejně tak dotazovatelný jako JSON / CBOR / CLI.

#### MVP System Graph

Ukazuje:

- co běží,
- které služby existují,
- které drivery ovládají která zařízení,
- která komponenta má jaké capabilities,
- jaké jsou závislosti,
- co spadlo,
- co se restartovalo.

#### Pozdější rozšíření: Flow Graph

Později zvážit vizualizaci datových toků:

```text
uzel = aplikace/služba/driver/device/volume
hrana = komunikace nebo datový tok
šířka hrany = kapacita
vyplnění = aktuální využití
barva/stav = OK / varování / přetížení / chyba
```

Příklad:

```text
VideoPlayer -> VideoDecoder
  420 MB/s z 500 MB/s

VideoDecoder -> GPU
  480 MB/s z 500 MB/s

StorageService -> NVMe driver
  80 MB/s z 3500 MB/s
```

Cílem je vidět bottlenecky, fronty, latency a vytížení „trubek“ mezi komponentami.

Flow Graph je odložený do pozdější fáze. System Graph jako základní přehled by měl být od začátku.

---

## 4. Úložiště a data

### Storage model

Storage model je jeden ze zásadních rozdílů proti Linuxu. Hlavní záměr je jednoduchý: **každá cesta patří jednoznačně jednomu volume a nikdy se nesmí potichu sáhnout na jiný disk.**

#### Hlavní princip

```text
Cesta vždy patří přesně jednomu volume.
Pokud volume není dostupné, operace selže (nezapisuje se jinam).
Pojmenování dat je oddělené od fyzického umístění.
```

#### Proč ne mount pointy a globální root strom

- **Design.** Mountování zařízení a filesystémů do jednoho sdíleného stromu je dlouho překonaný model - míchat fyzická zařízení a jejich filesystémy do společné adresářové struktury je nepořádek, který by moderní systém dělat neměl.
- **Bezpečnost.** V modelu, kde se filesystémy „vmountují" do sdíleného globálního stromu (`/mnt/...`, `/media/...`), platí:
  - Po odpojení volume zůstane prázdný adresář a zápis tiše skončí na *jiném* (nebo systémovém) disku.
  - Stejná cesta může v čase znamenat různá zařízení podle toho, co je zrovna připojené.
  - Míchání zařízení a filesystémů do jednoho stromu stírá hranici „kde data fyzicky jsou".

Proto OS používá explicitní volumes a volume-relativní cesty: identita úložiště je součástí cesty, takže „omylem na špatný disk" je strukturálně nemožné.

#### Oddělení vrstev

Rozlišujeme:

```text
Disk       = fyzické zařízení
Partition  = oblast na disku
Volume     = filesystem / datový prostor
Path       = cesta uvnitř konkrétního volume
```

#### Storage/admin namespace

Administrace disků, partitions a volumes:

```text
storage://disk/nvme0
storage://disk/nvme0/partition/1
storage://partition/gpt/<id>
storage://volume/<uuid>
```

`storage://` není běžná cesta k uživatelským datům. Je to administrační namespace.

#### Data namespace

Kanonická adresa dat je vždy navázaná na **jednoznačnou identitu volume (UUID)**, ne na lidský název:

```text
vol://<volume-uuid>/path/to/file
```

Příklad:

```text
vol://7a1f91c2-4d10-4a2a-a57e-f21c00112233/Documents/book.pdf
```

`vol://<uuid>` je **úniková / skriptovací forma** - vždy jednoznačná, nikdy nevede na špatný disk. Pro každodenní práci aplikace nepracuje s UUID napřímo, ale s **jmény ve svém per-proces namespace** (`user://`, `appdata://`, …), která se přes capability resolvují na konkrétní volume (viz níže).

Petname (lidský štítek) se v kanonické cestě **nikdy neobjeví** - protože nemusí být unikátní, nemá žádnou `vol://`/URI formu a nedá se přes něj adresovat (viz *Lidsky přívětivé pojmenování*).

#### Lidsky přívětivé pojmenování (vrstvy identity)

UUID je sice jednoznačné, ale jako primární UX je nepoužitelné - nikdo nechce psát `vol://7a1f91c2-…/…`. Pojmenování je proto rozvrstvené a každá vrstva řeší jen jednu věc:

| Vrstva | Co to je | Vlastnost | Důvěra / použití |
|---|---|---|---|
| **UUID** | trvalá identita volume (v metadatech) | globálně unikátní, neměnné | zdroj pravdy pro resolution, jediné, co je v `vol://` |
| **Petname** | lidský popisek volume (`backup-ssd`) | **nemusí být unikátní**, jen pro zobrazení | nikdy se přes něj neadresuje ani neresolvuje, nemá URI formu |
| **Self-label** | jméno zapsané ve volume při formátu (`Samsung-T7`) | jen nápověda | nedůvěryhodné, pouze informativní |
| **Jméno v per-proces namespace** | co aplikace reálně vidí (`user://`, `appdata://`) | unikátní *uvnitř* daného namespace | přiděleno capabilitou, resolvuje na konkrétní volume |

Klíčová pravidla:

- **Resolution probíhá vždy přes UUID/capability, nikdy přes petname.** Petname je čistě zobrazovací popisek - nelze přes něj nic otevřít ani adresovat.
- **Petname nemusí být unikátní - a to je záměr.** Když uživatel připojí USB disk se stejným petname jako jiný disk, nic se neděje a systém nic „nevyhazuje“ na konzoli: petname totiž nikdy neurčuje, kam se sáhá. Žádné přejmenovávání, žádný konflikt k řešení. Dva disky se stejným petname se v UI prostě odliší dalšími údaji (self-label, prefix UUID, kapacita, připojení).
- **Self-label se nikdy nedůvěřuje.** Je to jen informativní nápověda zapsaná ve volume, na resolution nemá žádný vliv.
- **Aplikace nevidí globální seznam disků.** Při startu dostane per-proces namespace (model Plan 9 / Fuchsia): mapování logických jmen na *konkrétní* volume capability. K čemu nedostala capability, to neumí ani pojmenovat. Tato jména jsou unikátní uvnitř namespace, protože je spravuje jeho vlastník. **Pozor: tohle omezení platí pro aplikace, ne pro uživatele** - uživatel přes důvěryhodný správce souborů / shell vidí a spravuje všechny disky (viz *Ergonomie*, role uživatel vs. aplikace).
- **UUID se uživateli běžně nezobrazuje.** V CLI/adminu se ukazuje petname + self-label + krátký prefix UUID pro odlišení, např. `backup-ssd (Samsung-T7, 7a1f…2233)`. Plné UUID jen v `storage://` a `vol://`.
- **`vol://<uuid>` je jediná „úniková“ forma adresování podle holé identity** - pro skripty a recovery, ne pro každodenní psaní.

Pro uživatelsky řízený přístup k souborům slouží **file picker / powerbox**: uživatel vybere soubor v důvěryhodném systémovém dialogu a aplikace dostane capability (handle) přesně na to místo - aniž by volume vůbec pojmenovávala. (Detaily v sekci o bezpečnostním modelu.)

#### Cesta je objekt, URI je jen reprezentace

Schémata jako `vol://`, `user://` nebo `storage://` vypadají jako URL, ale **kanonická forma cesty není textový string - je to typovaný objekt.** URI je jen jedna z jeho reprezentací, přesně v duchu pravidla ze sekce *System API model* („jedno typované API, víc reprezentací“).

U „cesty“ se obvykle do jednoho stringu slévají tři různé věci, které je potřeba rozlišit:

| Vrstva | Co to je | Forma |
|---|---|---|
| **Autorita** | čím se zdroj reálně otevře | **capability / handle** (nepodvrhnutelný odkaz na objekt) |
| **Kanonická hodnota** | co se předává v API | **typovaný objekt** (record/variant z IDL) |
| **Reprezentace** | jak se hodnota zobrazí/zapíše | URI text, JSON, CBOR, binárka |

Kanonické typy (návrh):

```text
VolumeId     = { uuid: Uuid }                              // jednoznačná identita
Segment      = neprázdný název bez „/“, „.“ a „..“
RelativePath = [Segment]                                   // seznam segmentů, ne string
VolumePath   = { volume: VolumeId, path: RelativePath }
NsName       = { namespace: NsKind, path: RelativePath }   // user://, appdata://, …
DeviceRef    = variant { Disk(id) | Partition(id) | Volume(VolumeId) }  // storage://
```

Stejná hodnota má víc reprezentací:

```text
objekt:  VolumePath { volume: { uuid: 7a1f… }, path: ["Documents", "book.pdf"] }
URI:     vol://7a1f…/Documents/book.pdf
JSON:    {"volume":{"uuid":"7a1f…"},"path":["Documents","book.pdf"]}
```

Co tím získáváme:

- **Autorita není ve jméně.** String (v jakékoli reprezentaci) sám o sobě nic neotevře, otevírá se přes capability na namespace, který proces už drží. Tím odpadá *confused deputy* i „resoluce proti globálnímu rootu“.
- **Odolnost proti path-traversalu na úrovni typu.** `RelativePath` je seznam validovaných segmentů, ne string - `..`/`/`-injection nemá kde vzniknout (klasický zdroj děr u stringových cest).
- **URI zůstává jako pohodlná textová serializace** pro shell, config a log. Je to plnohodnotná reprezentace (má gramatiku `scheme://authority/path`), jen není *modelem* - modelem je objekt.

Pravidlo: **objekt je kanon, URI/JSON/CBOR/binárka jsou jeho reprezentace, autorita je vždy v capability.**

#### Logické namespaces

Byly dohodnuty tyto logické namespaces:

```text
system://
apps://
user://
appdata://
cache://
runtime://
vol://
storage://
```

Význam:

| Namespace | Význam |
|---|---|
| `system://` | systémové soubory / základ OS |
| `apps://` | instalované aplikace |
| `user://` | uživatelská data |
| `appdata://` | per-app persistentní data |
| `cache://` | mazatelná cache |
| `runtime://` | dočasný runtime stav |
| `vol://` | explicitní volume podle UUID |
| `storage://` | administrace disků/partitions/volumes |

Důležité: tyto namespaces nejsou mount pointy. Jsou to logické resolvery nad storage a capability modelem.

#### Ergonomie: práce napříč volumes a UX pojmenování

Explicitní volumes řeší bezpečnost (nikdy se tiše nesáhne na špatný disk), ale nesmí z běžných operací udělat utrpení. Cílová vize ergonomie:

**Operace napříč volumes (přesun/kopie z disku A na disk B).**
Koordinuje je **StorageService**, ne aplikace ručně. Aplikace drží capability na zdroj i cíl (typicky z file pickeru) a zavolá jednu typovanou operaci:

```text
StorageService.Transfer(src: FileCapability, dst: DirCapability, mode: Move | Copy)
```

- Service drží obě volume capability a provede přenos jako **jednu sledovatelnou operaci** (progress, zrušení, obnovení) - žádné „aplikace si to přebírá byte po bytu".
- Přesun *uvnitř* jednoho volume je atomický rename, přesun *mezi* volumes je copy + verify + delete, protože jde fyzicky o dvě zařízení. Tato hranice je explicitní, ne skrytá.
- Aplikace nikdy nepotřebuje globální pohled na disky - stačí jí dvě konkrétní capability.

**Jednotný „domov" přes víc zařízení (náhrada symlinků/overlay).**
Místo tichého slévání disků do jednoho stromu (klasický overlay, kde není známo, kde data fyzicky jsou) řešíme „jeden logický domov" **explicitní kompozicí na úrovni namespace**:

```text
user:// není jeden disk, ale typovaný pohled složený z explicitně přidaných volumes.
Každá položka v user:// ví, na kterém volume fyzicky leží.
Skladbu pohledu vlastní uživatel/služba, ne náhoda připojení.
```

Tím se zachová pohodlí („mám jeden Domov"), ale **nikdy se neztratí informace, kde data reálně jsou** - opak overlay/mount modelu.

**Záloha a sync bez globálního pohledu na disky.**
Zálohu nedělá nikdo, kdo „vidí všechny disky" (to je přesně ta ambient authority, které se zbavujeme). Dělá ji **služba s explicitně předanými capability** na zdrojové a cílové volumes:

```text
BackupService dostane capability na zdrojové volumes + cílové volume.
Vidí přesně to, co jí bylo předáno - nic víc.
Snapshot/checksum/inkrementální sync jsou vlastnosti FS backendu (viz Native FS).
```

**Mentální model uživatele „kde jsou moje soubory".**

**Capability a namespace model:**

- omezuje aplikace (dostane jen to, co jí předáme)
- neomezuje uživatele (ten má plnou kontrolu)

**„Systémová aplikace" není zvláštní privilegovaná třída.** Správce souborů, shell ani správce disků nejsou „jiný druh" softwaru než aplikace třetí strany - capabilities dostávají **úplně stejným mechanismem**. Liší se jen *tím, jaké capabilities dostaly*, ne *tím, čím jsou*.

- žádný uid 0
- žádná „systémová" výjimka
- žádné ambient právo navázané na původ binárky

- **Širokou kontrolu nad disky má ta aplikace, které ji systém/uživatel udělil.** Typicky správce souborů nebo shell - dostane široké storage capabilities (všechny disky, libovolné volumes, procházení i vytváření struktury), a přes ně má uživatel plnou kontrolu. „Důvěryhodný nástroj" tu znamená právě a jen „dostal širokou capability", ne vestavěný privilegovaný status.
- **Stejné capabilities může dostat i aplikace třetí strany.** Vlastní správce souborů od uživatele dostane *přesně totéž* co vestavěný - mechanismus je jeden jediný. A naopak: vestavěná appka s úzkou capability nemá víc práv než kdokoli jiný.
- **Většina aplikací dostane jen úzké capabilities.** Nevidí globální seznam disků, dostanou jen to, co jim uživatel předal (typicky jeden soubor/složku přes file picker). To není omezení uživatele - to je ochrana uživatele *před aplikacemi*.

Jak to drží pohromadě (žádný „root"):

- Privilegium **není vlastnost procesu** („jsem systémová aplikace"), ale **vlastnost držené capability**.
- Široký přístup drží konkrétní nástroj proto, že mu byl **explicitně udělen** (při instalaci nebo uživatelem v session) - auditovatelně a odvolatelně, ne ambient authority.
- Uživatel z té široké autority pak *deleguje úzké řezy* dál (jeden soubor, jedna složka) přes picker.

**`user://` je výchozí pohodlí, ne klec.**

- Pro **každodenní běžný tok** (a pro laika) je `user://` Domov + file picker pohodlný default: uživatel nemusí řešit volumes ani UUID, prostě „Dokumenty", „Stažené".
- **Pokročilý uživatel ale na `user://` zamčený není** - přes správce souborů/shell se dostane na `storage://`, konkrétní `vol://`, jiné disky a skladá si vlastní strukturu. `user://` je jen jeden (výchozí) pohled, ne hranice toho, co uživatel smí.
- Když je víc zařízení se stejným petname, UI je odliší doplňujícími údaji (self-label, kapacita, připojení, krátký prefix UUID).

Shrnuto: **omezuje se aplikace, ne uživatel.** Model zůstává přísný vůči aplikacím (autorita v capability, identita v UUID), ale uživatel, přes důvěryhodné nástroje, má plnou Windows-like kontrolu nad svými disky. Ergonomii laikovi dodává `user://` + picker jako default, ne jako klec. Detailní návrh těchto operací patří do pozdější fáze - tady je zafixovaný směr, ne API.

---

### Native filesystem

Storage model je rozhodnutý, ale nativní filesystem ještě není detailně navržen.

#### Podporované kompatibilní FS později

- ext4,
- NTFS,
- exFAT,
- FAT32,
- ISO9660,
- UDF.

Tyto filesystémy jsou backendy za jednotným Volume API.

#### Nativní FS později

Pracovní názvy:

```text
ModernFS
LiberFS
NovaFS
```

Možné vlastnosti:

- copy-on-write,
- checksums,
- snapshots,
- encryption,
- compression,
- atomic writes,
- typed metadata,
- rollback.

Pro MVP stačí jednodušší FS nebo ramdisk/init package.

---

## 5. Bezpečnost a aktualizace

### Bezpečnostní model: aktuální rozhodnutí

Capabilities jsou pevný základ.

Ale přísný aplikační sandbox a detailní permission manifesty nejsou povinné pro první MVP.

#### Pro MVP

Hranice je jasná: **žádná ambient authority už od MVP.** Co se odkládá, je *granularita* policy a manifestů - ne samotná izolace.

```text
TVRDÉ PRAVIDLO (platí od MVP):
komponenta/služba dostane JEN explicitně předané capability, nic víc.
Žádný globální přístup k FS, zařízením ani jiným službám „defaultně".
```

- **Tohle máme z WASI/capability modelu prakticky zadarmo** - Wasm komponenta nemá ambient authority z principu, takže není důvod ji v MVP změkčovat.
- Důvod přísnosti od začátku: kdyby si kód zvykl na ambient authority, pozdější retrofit sandboxu je bolestivý (přesně proto to Android/iOS dělají hned). Capability model bez vynuceného „nic navíc" je z velké části jen jiná syntaxe.

```text
ODLOŽENO na později (ne do MVP):
- detailní granularita oprávnění a permission manifestů,
- jemné portály (mic/cam/screenshot), síťové politiky,
- plný audit a policy management.
```

#### Později

- přísný app sandbox,
- detailní permission manifesty (typovaný objekt `PermissionSet` / `Manifest`, ne textový soubor - viz *System API model*),
- file picker vracející file handle,
- síťová oprávnění,
- mic/camera/screenshot portály,
- detailní audit capabilities.

---

### Immutable systém a update model

Immutable signed system, A/B updates, rollback a verified boot jsou považovány za správný moderní směr, ale ne za povinný blokér MVP.

#### Odloženo do pozdější fáze / ke zvážení

- immutable `system://`,
- signed system image,
- A/B updates,
- rollback,
- verified boot,
- package trust chain,
- šifrované user volumes.

Pro první verzi může být systém jednodušší.

---

## 6. Rozhraní a aplikační model

### Aplikační model: nativní ABI + WebAssembly/WASI host

**Výchozím a stabilním aplikačním kontraktem je nativní typované capability IPC/ABI** - totéž ABI, přes které mluví kernel, drivery a core služby:

- Stavíme ho tak jako tak, takže je to **default i pro aplikace**, ne jen pro systémové části.
- Aplikace nestavíme na nativním ELF + POSIX (to záměrně ne), ale ani z nich neděláme výjimku s vlastním nezávislým kontraktem.

**Nad tímto nativním ABI je WebAssembly Component Model + WASI první a doporučený aplikační host:**

- Aplikace píšeme přednostně jako Wasm komponenty - odvážné, ale záměrné moderní rozhodnutí: z Wasm/WASI plyne sandbox, přenositelnost a jazyková neutralita prakticky zadarmo.
- Klíčové ale je, že v souladu s *Principem vrstvení* je **WASI jen jeden z hostů nad stabilním nativním kontraktem, ne *definice* systému** - systém na něm *nestojí* a nezávisí na jeho zralosti (rozvedeno níže v *WASI jako jeden z hostů*).
- Proč takhle (a ne „Wasm je celý aplikační model"): tím, že nativní ABI je default i pro aplikace, **klesá závislost na ještě nezralých částech WASI** (GUI, async, threading). Co WASI zatím neumí čistě, jde mezitím řešit přímo přes nativní ABI, bez čekání na stabilizaci cizího spec.

#### Proč WASI/komponenty

- **Capability-based už z principu.** WASI (preview 2) nemá ambient authority - komponenta dostane jen importy/capabilities, které jí předáme. To 1:1 sedí na kernelový capability model.
- **Sandbox by default.** Wasm lineární paměť je izolovaná, aplikace je odstíněná ještě nad rámec procesové izolace (obrana do hloubky).
- **Jazyková neutralita.** Rust, C, C++, Go a další se kompilují do Wasm. Vývojář si jazyk nevolí podle OS.
- **Přenositelnost.** Jeden binární artefakt běží na x86-64, ARM64 i RISC-V. To výrazně zmírňuje problém „ekosystém od nuly".
- **Sjednocení s IDL.** WIT (rozhraní komponent) může být přímo náš IDL (viz sekce IDL).

#### Jak to zapadá do systému

```text
Nativní (Rust) procesy:   kernel, drivery, core služby (Storage/Net/…).
WASM komponenty:          APLIKACE (a postupně i vyšší služby).
WASI host:                runtime proces, který mapuje WASI importy
                          na náš typovaný service API přes IPC kanály.
```

- **WASI „world" = sada capabilit**, kterou komponenta dostane při startu (filesystem handle z file pickeru, socket od NetworkService, …).
- **WASI importy implementují naše služby.** Např. `wasi:filesystem` voláme přes Channel na StorageService, `wasi:sockets` na NetworkService.
- **Výkon:** komponenty lze interpretovat/JIT (Wasmtime/Cranelift) pro přenositelnost, nebo **AOT zkompilovat při instalaci** pro rychlost.

#### WASI jako jeden z hostů nad stabilním nativním ABI

Toto je klíčové architektonické rozhodnutí, které řeší hlavní riziko sázky na WASI (Component Model i WASI jsou mladé a vyvíjejí se):

```text
Stabilní základ = nativní typované capability IPC/ABI (náš vlastní, z IDL).
WASI host       = vrstva NAD tímto základem, která mapuje wasi:* importy
                  na náš service API. Jeden z možných hostů, ne jediný.
```

Proč zrovna takhle:

- **Systém není svázaný s pohyblivým spec.** Když se WASI/Component Model změní, přepíše se **WASI host adaptér**, ne kernel, služby ani jejich kontrakty. Riziko je izolované do jedné vyměnitelné vrstvy.
- **Víc aplikačních modelů může koexistovat.** Nad stejným nativním ABI může vedle WASI hosta vzniknout pozdější nativní app ABI, POSIX-like shim (viz *Kompatibilita*) nebo jiný runtime - bez přepisování systému.
- **Výhody WASI zůstávají tam, kde se hodí** (sandbox, přenositelnost, jazyková neutralita pro aplikace), ale neplatíme za ně tím, že by celý OS závisel na cizím, ještě neustáleném standardu.
- **Sedí to na *Princip vrstvení*** (sekce Úvod a principy): závislost je na kontraktu, ne na konkrétní implementaci hosta.

Jinými slovy: **nativní ABI je „plán A" i „plán B" zároveň**:

- stavíme ho tak jako tak (mluví přes něj kernel, drivery a core služby)
- systém na WASI *nestojí*
- systém stojí na vlastním IPC (WASI je jeho první a doporučený aplikační konzument).

#### Poctivé kompromisy

- Wasm má režii oproti čistě nativnímu kódu (zmírnitelné AOT).
- Component Model je mladý a vyvíjí se.
- Výkonově extrémní nebo nízkolatenční úlohy (GPU, ovladače) zůstávají nativní - Wasm je vrstva *aplikací*, ne celého systému.
- **Default kontrakt je nativní typované ABI** (stavíme ho tak jako tak pro kernel, drivery a služby) a je dostupný i aplikacím. **Wasm komponenta je první a doporučená cesta pro aplikace** nad tímto ABI, speciální nebo výkonově citlivé aplikace mohou jít přímo přes nativní ABI.

---

### IDL jazyk

Nutné ještě navrhnout.

Cílem je formální popis systémových API.

Příklad:

```text
interface Storage.Volume {
  Open(path: RelativePath, rights: Rights) -> FileHandle
  Stat(path: RelativePath) -> FileInfo
  Watch(path: RelativePath) -> EventStream<FileEvent>
}
```

Z IDL se má generovat:

- binary IPC layout,
- CBOR schema,
- JSON schema,
- Rust klient,
- případně C ABI binding,
- CLI formatter,
- dokumentace,
- compatibility testy.

Toto je klíčové, aby API nezdegenerovalo do chaosu.

#### Vztah k WIT (WebAssembly Interface Types)

Protože aplikační model staví na WebAssembly komponentách (viz *Aplikační model*), je silný kandidát **přijmout WIT jako IDL** místo vymýšlení vlastního jazyka - nebo nechat vlastní IDL a generovat z/do WIT. Výhody WIT:

- už řeší typy, interface, světy (worlds) a verzování,
- má nástroje (`wit-bindgen`) generující bindingy do více jazyků,
- přirozeně sedí na capability model (importy = capabilities).

Vlastní binární IPC layout, CBOR/JSON reprezentace a CLI formatter pak mohou být *backendy* nad WIT popisem. Rozhodnutí mezi „vlastní IDL“ a „WIT jako IDL“ je otevřené, ale směr je sjednotit IDL s WIT, ne udržovat dva paralelní systémy.

#### Rozhodnutí až po reálné zkoušce, ne dopředu

WIT nebyl navržen jako nízkoúrovňový systémový IPC IDL - a věci jako předávání kernel capabilit, zero-copy shared buffery, DMA handle nebo async streamy s backpressure se do něj nemusí mapovat čistě (Fuchsia proto schválně postavila vlastní FIDL). Proto **definitivní volbu „WIT vs. něco jiného hotového vs. vlastní IDL“ neděláme teď od stolu.**

```text
POSTUP:
1. Napsat 5-6 REÁLNÝCH interface ve WIT, ne hello-world:
   Storage.Volume, Process, Log,
   Channel s předáním handle,
   EventStream s backpressure (+ např. Transfer napříč volumes).
2. Zjistit, kde to drhne (handle passing, zero-copy, streamy, ABI stabilita).
3. Teprve podle té zkušenosti rozhodnout.
```

Pravděpodobný kompromis (k ověření, ne dogma): **WIT jako zdroj typů a rozhraní, vlastní binární layout + handle tabulka jako backend.** Ale potvrdí se to až praxí na reálných interface, ne předem.

---

### Kompatibilita a POSIX-like vrstva (odloženo)

Systém je primárně **vlastní** (typované capability API + WASI). POSIX **není** cílem jádra. Přesto kompatibilitu nezahazujeme - jen ji řešíme správně a později.

#### Princip

POSIX-like kompatibilita je **volitelná userspace vrstva**, ne součást kernelu:

- Překladová vrstva (libc + emulace syscallů), která mapuje POSIX volání na naše nativní služby.
- Žádné POSIX primitivum se nedostane do jádra.

#### Možné úrovně (od nejjednodušší)

```text
1. WASI -> POSIX shim:        POSIX-like API pro Wasm komponenty.
2. relibc-style libc:         nativní libc nad našimi službami
                              (model Redox relibc) pro porty programů.
3. Linux-syscall emulace:     spouštění nemodifikovaných Linux binárek
                              (model Fuchsia Starnix / WSL1) - nejnáročnější,
                              nejpozdější fáze.
```

#### Pořadí: WASI first, POSIX-like later

Nejdřív pořádně postavíme nativní a WASI cestu. Kompatibilní vrstva přijde, až bude co a proč portovat - jako pohodlí pro vývojáře, ne jako berlička, která rozmělní nativní model.

> Pozn. k formulaci: ano, je to „vlastní vrstva kompatibility" - konkrétně **userspace překladová vrstva**, která POSIX/Linux rozhraní převádí na naše služby. Nejde o to dělat z OS Linux, ale umět na něm *spustit* existující software, když to dává smysl.

---

## 7. Roadmapa a závěr

### MVP návrh

První praktická verze OS by měla umět:

```text
boot v QEMU
serial log
framebuffer text output
physical memory manager
virtual memory
heap allocator
userspace address space
thread
scheduler
channel IPC
handle table
basic capabilities
spustit SystemManager
poslat první zprávu přes IPC
zachytit page fault userspace procesu
uklidit spadlý proces
ramdisk/init package
StorageService nad ramdiskem
vol:// přístup
jednoduché CLI
základní System Graph
```

**Ovladače v MVP:** pouze virtio (viz sekce Drivery). **Aplikační ABI je rozhodnuté** - WebAssembly komponenty + WASI (viz *Aplikační model*), jakmile poběží core IPC a služby, je blízkým cílem **minimální WASI host, který spustí první komponentu**. MVP samotné ale stojí na nativních Rust službách, Wasm host přichází hned v navazující fázi (viz *Roadmapa*).

Záměrně neřešit v MVP:

```text
GPU akceleraci
USB
síť
NVMe
plný filesystem
GUI
package manager
přísný app sandbox
immutable update
verified boot
Flow Graph metriky
```

---

### Roadmapa

Roadmapa je milníková, ne časová (záměrně bez termínů):

- Rozsah řídíme přes fáze a každá fáze má být *použitelný* mezistav.
- Pořadí fází sleduje nasazovací cíle appliance/edge → server → desktop (viz *Proč tento OS místo Linuxu*).
- Nasazení na reálný hardware přichází po serverové fázi, AI platforma jako závěrečná evoluce nad desktopem.

**Jak číst horizont fází.** Fáze 0-2 cílí na appliance/edge a představují **reálný, blízký cíl** jednoho člověka nebo malého týmu (bootovatelný capability microkernel + první WASI komponenta + virtio + síť). Je třeba je chápat jako *úplný* projekt, nikoli jako odrazový můstek k něčemu většímu - i samotná appliance/edge platforma je dokončený, smysluplný produkt.

**Fáze 3-6 nejsou plánem, ale vizí - a platí pouze za předpokladu, že kolem projektu vznikne komunita.** Fáze 3 (server), Fáze 4 (reálný hardware), Fáze 5 (plnohodnotný desktop) a Fáze 6 (AI platforma) představují stovky člověko-roků. Jsou proto vědomě formulovány jako *směr*, kam systém **může** růst díky své architektuře s příchodem dalších přispěvatelů.

**Co komunitu přitáhne a co nikoliv**:
- NE - modernost a absence legacy
- ANO - capability-based bezpečnost zabudovaná do základu systému (žádná ambient authority, žádné „root může vše") - strukturální záruka, kterou do Linuxu kvůli 30 letům zpětné kompatibility lze jen velmi obtížně doplnit. Ostatní pilíře (aplikační model WASI, paměťová bezpečnost) ji podpírají, avšak právě tato jediná vlastnost je důvodem, proč by se k projektu někdo vůbec připojil a začal budovat ekosystém.

#### Fáze 0 - Bring-up (MVP jádra)

```text
boot v QEMU (Limine), serial log, framebuffer text
physical/virtual memory, heap, address spaces
thread, scheduler (SMP-aware návrh, běh zatím na jednom jádře), Channel IPC, handle table, capabilities, Domain
start SystemManager, první IPC zpráva
zachycení page faultu, úklid spadlého procesu
ramdisk/init package, StorageService nad ramdiskem, vol:// přístup
jednoduché CLI, základní System Graph
```

#### Fáze 1 - První použitelný userspace

```text
IDL/WIT toolchain a generátory
core služby: Process, Storage, Log, Device, Config
virtio drivery (headless): blk, net, console
minimální WASI host: spuštění první Wasm komponenty
prototyp file pickeru (powerbox)
```

#### Fáze 2 - Appliance/edge platforma

```text
síťový stack nad virtio-net (priorita - na edge je síť jádro)
observabilita a remote admin: plný System Graph, JSON/CBOR/CLI reprezentace, tracing, counters
bezpečnostní hardening: app sandbox, permission manifesty, threat model
ServiceManager s restart policy a watchdog
plný Component Model + WASI preview 2, SDK pro Rust/C/Go
package/app formát, instalace, AOT kompilace
jednoduchý perzistentní nativní filesystem
```

---

> **Od tohoto bodu dále: pouze vize.** Fáze 3-6 níže platí pouze za předpokladu, že kolem projektu vznikne komunita. Jde pouze o mapu směru, kam systém *může* růst s příchodem dalších přispěvatelů, nikoli o pevný plán.

#### Fáze 3 - Serverová platforma

```text
POSIX-like kompatibilní vrstva (relibc-style) - pro cizí serverový software
uživatelské účty / identity (víceuživatelská správa, vzdálený přístup) - userspace identita nad capabilitami, ne kernel uid/gid
lokalizace (locale, jazyk, časová zóna, formátování) - relevantní už v CLI a v logách
širší síťový stack a server-class workloady
immutable signed systém, A/B updates, rollback, verified boot
šifrované user volumes
nativní moderní FS (CoW, checksums, snapshots, komprese)
```

#### Fáze 4 - Reálný hardware (nasazení na reálné servery/SBC)

```text
driver binding model v praxi: DeviceManager páruje reálná zařízení → drivery
výběrové ovladače reálného HW dle nasazení (NVMe, NIC, úložiště, sběrnice)
podpora konkrétních serverů a SBC (single-board computers)
ARM64 / RISC-V desky vedle x86-64
power management dle nasazení (ACPI, idle/suspend)
přechod z virtio/VM na bare metal
```

#### Fáze 5 - Desktopová platforma

```text
GUI/compositor (virtio-gpu i reálné GPU), vstup: klávesnice/myš/touch
správce oken (window manager), desktop shell a kompletní uživatelské prostředí
audio stack (přehrávání i záznam)
portály: mic/cam/screenshot, sdílení obrazovky, výběr souborů
package manager / app store pro koncové uživatele
uživatelské profily a nastavení desktopu, přístupnost (čtečky obrazovky apod.)
notifikace, schránka, drag-and-drop, podpora více monitorů
akcelerovaná grafika a multimédia
volitelná emulace Linux binárek (Starnix-style) - běh existujících aplikací
Flow Graph metriky
```

**Teprve v této fázi se „přívětivost pro běžné uživatele" stává reálným cílem.** Jde o vyvrcholení trajektorie developer-first → ekosystém → široká přívětivost (viz *Proč tento OS místo Linuxu*): běžný uživatel přichází *až* ke zralému desktopu s ekosystémem a aplikacemi, nikoli k holému jádru. Do té doby běžný uživatel není cílovou skupinou, podle níž se činí raná návrhová rozhodnutí.

#### Fáze 6 - AI platforma

Alternativa ke klasickému desktopu:

- primárním rozhraním není přímé ovládání aplikací, ale AI, která za uživatele vykonává jeho záměry.
- staví až na desktopu (Fáze 5), protože jde o **virtuálního agenta (3D avatar)** - vizuálního a hlasového agenta, který vedle sebe uživateli zobrazuje obsah relevantní ke konverzaci (text, video, zvuk, obrázky).
- potřebuje kompletní grafický, audio a multimediální základ z desktopové fáze
- capability model a typované API (*objekt je kanon*) k tomu dělají ze systému bezpečný a strojově ovladatelný substrát pro takového agenta
- mimo lokální systém se agent napojuje i na externí nástroje a služby standardním protokolem (**MCP - Model Context Protocol**), každý takový konektor je ale jen další capability-omezená komponenta, takže propojení s vnějškem nerozšiřuje jeho oprávnění za hranice udělené uživatelem.

```text
ztělesněný virtuální agent (3D avatar) + hlasový a textový vstup/výstup jako primární rozhraní
prezentace nalezeného obsahu vedle agenta: text, video, zvuk, obrázky (multimédia z Fáze 5)
AI rozhraní jako primární způsob interakce - uživatel formuluje záměr, neovládá aplikace přímo
AI agent vykonává požadavky za uživatele přes typované systémové API a aplikace
capability-omezený agent: jedná jen v rámci udělených oprávnění (auditovatelně, odvolatelně)
orchestrace aplikací a služeb AI vrstvou nad typovaným objektovým API
propojení s externími nástroji, daty a službami přes MCP (Model Context Protocol) - jednotný protokol, kterým agent volá vzdálené nástroje a API
každý MCP konektor běží jako samostatná capability-omezená komponenta (sandbox, auditovatelně, odvolatelně)
portály a potvrzování citlivých akcí - AI nesmí překročit udělené capabilities
audit akcí AI přes System Graph a capability model
klasický desktop zůstává dostupný jako alternativní rozhraní
```

---

### Licence

Projekt je **open source pod licencí Unlicense** (uvolnění do public domain).

- **Maximální volnost:** kdokoli smí kód použít, upravit, distribuovat, komercializovat i uzavřít odvozené dílo, bez podmínek a bez nutnosti uvádět autorství.
- **Žádný copyleft, žádná atribuce** - záměrně nejnižší možná bariéra pro přijetí a forkování.
- **Příspěvky** se přijímají pod Unlicense, vhodné je doplnit DCO/poznámku, že přispěvatel s tím souhlasí.
- **Třetí strany:** vlastní kód je Unlicense, ale převzaté komponenty si nesou své (permisivní) licence - např. Wasmtime (Apache-2.0), Limine (BSD). To je v pořádku, jen je nutné je evidovat.

---

### Otevřené otázky

Část původních otázek je nově rozhodnuta (viz výše). 

```text
ROZHODNUTO:
- sync vs async IPC ....... async jádro + sync-vypadající API (sekce IPC)
- bootloader .............. Limine (příp. UEFI) (sekce Boot flow)
- aplikační model ......... nativní typované ABI je default i pro aplikace,
                           WASI je první a doporučený host nad ním (sekce Aplikační model)
- IPC wire formát ......... default decode-cheap binárka (FIDL/Cap'n Proto styl),
                           ne CBOR/JSON (sekce IPC model)
- princip vrstvení ........ vyměnitelné vrstvy přes stabilní kontrakty (sekce Úvod a principy)
- capability model ........ detailní návrh (sekce Capability model)
- kernel object model ..... + Domain hierarchie (sekce Kernel object model)
- SMP / multicore ......... SMP-aware návrh od Fáze 0, optimalizace později (sekce Co je v kernelu)
- cesty/pojmenování ....... objekt je kanon, URI je reprezentace, petname jen popisek (sekce Storage model)
- objekt = kanon (všude) .. text/URI/JSON jsou reprezentace, autorita v capability (sekce System API model)
- ambient authority ....... žádná už od MVP, odložená je jen granularita (sekce Bezpečnostní model)
- licence ................. Unlicense (sekce Licence)
```

Stále otevřené:

1. Přesný binární IPC/message layout (*styl* rozhodnut - decode-cheap FIDL/Cap'n Proto, otevřené jsou jen konkrétní bajty).
2. IDL: vlastní vs. WIT vs. jiné hotové - **rozhodnout až po napsání 5-6 reálných interface** (sekce IDL), ne dopředu.
3. Event stream model do detailu.
4. Rozlišení Process vs Component vs Service vs Driver vs App v praxi.
5. ServiceManager/DeviceManager detailní návrh.
6. ResourceManager policy (limity, quotas).
7. Native filesystem (formát, vlastnosti).
8. GUI/compositor/input model.
9. Audio/video/network stack do detailu.
10. Přesná podoba System Graphu a později Flow metrics.
11. Verified boot / update model (immutable, A/B, rollback).
12. Power management (ACPI, suspend/resume, idle states) - nutné pro laptopy.
13. Testovací strategie (unit, integrace na QEMU, fuzzing syscallů, property testy capabilit).
14. Explicitní threat model (proti komu se bráníme: malicious app, compromised driver, …).
15. Driver binding model (jak DeviceManager páruje zařízení → driver).
16. Observabilita (counters, tracing spans, profiling napříč službami).
17. Chování při tlaku na paměť (reclaim, OOM přes Domain limity).

Pozn.: body 12-17 zatím nepotřebují detailní návrh - patří sem vědomě jako „nezapomenout", ne jako úkol do MVP. Většinu z nich rozhodne až praxe ve Fázi 0-1.

---

### Doporučený další krok

Object model, capability model i IPC model už mají základní návrh (viz výše). Další návrhové kroky:

```text
1. IDL/WIT: napsat pár reálných interface, podle nich rozhodnout, pak postavit toolchain (generátory bindingů).
2. Detailní návrh core služeb (Storage, Process, Log, Device).
3. Minimální WASI host a běh první komponenty.
4. virtio drivery (blk, console) pro reálné úložiště místo ramdisku.
5. SystemManager / ServiceManager / DeviceManager detailní návrh.
```

Nejbližší konkrétní krok (ne otázka od stolu): **napsat 5-6 reálných interface ve WIT** a empiricky zjistit, kde WIT drhne - teprve pak rozhodnout: WIT, jiné hotové, nebo vlastní IDL. Konkrétní seznam interface i postup viz *IDL jazyk*.
