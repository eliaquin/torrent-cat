# Seguimiento de robustez

Alcance: hallazgos F1–F15 de [la auditoría](BEP-AUDIT-2026-10-16.md).
Cada hallazgo sigue implementación → self-review → correcciones → pruebas → merge.
Las referencias de líneas de la auditoría describen el commit base, no el código posterior.

| Hallazgo | Estado | Evidencia |
| --- | --- | --- |
| F1 — aislamiento de archivos | Integrado en `df35676` y publicado; 35 pruebas pasan, 1 ignorada | `tests/storage_confinement.rs` |
| F2 — asociación de respuestas UDP | Integrado en `eb6252f` y publicado | Pruebas UDP del crate y `tracker_tests` |
| F3 — límites de entradas | Integrado en `4c970bd` y publicado | Parser, cuerpos HTTP, URL y tracker locales |
| F4 — integridad y durabilidad de reanudación | Integrado en `0b5d79c` y publicado | Hash completo, checkpoints y recheck manual |
| F5 — capacidad, cancelación y equidad de subida | Integrado en `a63b626` y publicado | Colas acotadas y peer loopback |
| F6 — niveles y failover de trackers | Integrado en `6a91e4e` y publicado | Niveles, promoción y barrera privada |
| F7 — recuperación y URLData UDP | Integrado en `edfed59` y publicado | Reloj simulado, UDP IPv4/IPv6 y ruta/query |
| F8 — extensiones y PEX | Integrado en `4515126` y publicado | Negociación por TCP y filtrado de payload PEX |
| F9 — ciclo de vida y persistencia | Integrado en `d4e988e` y publicado | Fallos de disco, reinicio abrupto, tracker silencioso y cierre de conexiones |
| F10 — contabilidad de escrituras parciales | Integrado en `ead35c1` y publicado | Transporte controlado, corrupción/repetición y anuncios locales |
| F11 — recuperación de metadata persistida | Integrado en `cd72889` y publicado | Entradas visibles, reparación por hash y cero descubrimiento |
| F12 — pausa y borrado durante inicialización | Integrado en `beef272` y publicado | Barrera de permisos, cancelación y stream de peers |
| F13 — límites al servir metadata | Integrado en `29932f9` y publicado | Índices extremos, bloques alineados y peer TCP |
| F14 — secuencia al recibir metadata | Integrado en `396c54c` y publicado | Negociación incremental y peers adverso/válido |
| F15 — buffers para mapas de piezas | Integrado en `d0a2899` y publicado | TCP grande, fragmentación y límites antes de reservar |

## F1: confinamiento de almacenamiento

Las operaciones sobre rutas de un torrent usan un descriptor de directorio compartido y `cap-std`, incluyendo creación, reapertura, restauración y borrado. Las lecturas y escrituras posteriores usan el descriptor del archivo abierto. La carpeta raíz elegida por el usuario es la frontera de confianza; los enlaces relativos que permanecen dentro de ella son admisibles. Esto no intenta aislar hardlinks ni un actor que pueda modificar directamente cualquier archivo del usuario.

Self-review: la primera implementación tenía un bypass de `O_NOFOLLOW` cuando la raíz terminaba en `/`, generado por `join("")`. La regresión falló; normalizar componentes antes de abrir la raíz corrigió el defecto. Se conserva la carpeta raíz vacía al borrar, porque resolver de nuevo su nombre podría borrar una carpeta distinta tras un rename. Los descendientes del torrent sí se eliminan. La raíz se fija al abrirla; la prueba de sustitución verifica que el borrado sigue el directorio original.

Pruebas: enlaces absolutos y relativos de archivo/directorio, creación y reutilización, raíz simbólica, restauración, sustitución de raíz/descendiente antes de borrar, y 500 aperturas/escrituras/borrados durante intercambios concurrentes. Los archivos externos conservan contenido, tamaño y existencia. Todas las pruebas utilizan directorios temporales sin trackers externos.

Validación final: `cargo test --manifest-path src-tauri/Cargo.toml --locked` — 29 pruebas de aplicación y 6 regresiones de almacenamiento pasan; una prueba de red pública ignorada. `cargo check` y `git diff --check` pasan. Sin hallazgos bloqueantes restantes en el diff F1.

## F2: asociación de respuestas UDP

Cada transacción reserva endpoint (IP y puerto), acción y receptor. Los mensajes malformados, de otro endpoint o de otra acción no consumen la transacción. Los errores del tracker se aceptan solo desde el endpoint esperado. Los errores remotos ya no se imprimen en la traza de recepción.

Self-review: mantener el ID reservado hasta que desaparezca su guard evita que la limpieza de una solicitud anterior elimine una nueva que haya reutilizado ese número. Se normalizan IPv4 mapeadas porque el socket dual-stack entrega IPv4 nativas; las IPv6 conservan su scope. La asociación no autentica UDP ni puede distinguir un replay idéntico desde el mismo endpoint si coincide de nuevo el ID aleatorio; no se afirma esa garantía.

Regresiones: IP/puerto/acción incorrectos, mensaje truncado, respuesta legítima posterior, ID desconocido, duplicado/tardío, reserva/reutilización y cancelación. Pruebas de socket reales en loopback IPv4 e IPv6. Referencia: [BEP 15](https://www.bittorrent.org/beps/bep_0015.html).

Validación final F2: `cargo test --manifest-path src-tauri/Cargo.toml --locked -p librqbit-tracker-comms --lib` — 7 pasan, 1 de red pública ignorada. `cargo test --manifest-path src-tauri/Cargo.toml --locked tracker_tests` — 9 pasan. Diff revisado sin hallazgos bloqueantes; `git diff --check` pasa.

## F3: presupuestos de entradas

Se vendoriza `librqbit-bencode` 9.0.1 (mismo commit upstream que el motor) para que las restricciones también cubran DHT, extensiones, metainfo, campos desconocidos y deserializadores directos. Presupuestos: 32 MiB de entrada bencode/metadata, 64 contenedores anidados y 1.000.000 de valores; las cadenas quedan limitadas por el presupuesto total. Conversión numérica comprobada, sin overflow ni acceso `unsafe` al extraer cadenas. Son límites de representación/trabajo, no una afirmación sobre el RSS total del proceso.

El tracker admite cuerpos HTTP de hasta 1 MiB, 2.000 peers por respuesta y un ID de hasta 4 KiB. HTTP se lee por fragmentos, sin reservar memoria a partir de Content-Length. Las descargas de metadata por URL tienen además un plazo de 30 s. Las lecturas locales/ stdin se detienen en el límite más un byte para detectar exceso. Los deadlines de anuncios usan suma comprobada y las respuestas inválidas siguen el backoff normal.

Self-review: además de las rutas de red, la lectura de `Core::add` comprobaba tamaño y luego leía sin límite, con una carrera si el archivo crecía. Ahora la lectura también queda acotada. Se conserva la compatibilidad de bencode con datos binarios tras un encabezado mediante `from_bytes_with_rest`, pero HTTP exige consumir toda la respuesta. La validación de intervalos ocurre antes de dar por exitoso el anuncio; una prueba recupera un tracker que primero devuelve `u64::MAX` y después una respuesta válida.

Validación: 21 pruebas de bencode y 10 de tracker pasan (1 de red pública ignorada); 30 pruebas de aplicación y 6 de almacenamiento pasan (1 pública ignorada), más la prueba de URL excesiva seguida de URL válida. Cuerpos chunked, EOF, longitud falsa/truncada, límite exacto y exceso; profundidad 50.000, campos ignorados, límite de valores, tamaños de cadena falsos y extremos numéricos. Clippy de bencode y tracker con `-D warnings` pasa. Diff contra las fuentes originales y el código de aplicación revisado sin hallazgos bloqueantes.

## F4: verificación completa y checkpoints ordenados

El arranque verifica todas las piezas del disco, independientemente de los bits persistidos. Se elimina el muestreo y la rama que interpretaba `Ok(false)` como éxito. También recupera piezas escritas después del último checkpoint. `Core` desactiva fast resume; la opción heredada del motor solo habilita checkpoints, nunca omite hashes. Una optimización basada en identidad/mtime de archivos queda deshabilitada: ni identidad ni tamaño sustituyen la comprobación de contenido. El coste es más lectura al reiniciar torrents grandes.

Con checkpoints habilitados: sincronizar payload → escribir snapshot temporal → sincronizar snapshot → rename atómico → sincronizar directorio. Si falla el primer paso, no se publica el mapa. Se elimina el worker con cola ilimitada y el flush automático al destruir el bitmap; no pueden persistir un estado que se salte la barrera. El mapa activo se construye desde los resultados verificados en memoria, sin volver a confiar en el archivo recién guardado. TorrentCat utiliza almacenamiento filesystem/JSON; el backend PostgreSQL opcional conserva su contrato, pero no se declara validado como producto.

El menú de un torrent pausado ofrece **Verify files**. Reabre los archivos mediante el directorio confinado, vuelve a comprobarlos y conserva ID, contadores, selección y pausa. Las llamadas sobre torrents activos se rechazan antes de esperar tareas de tracker. La operación no inicia descubrimiento ni transferencias.

Self-review: se eliminó el read-back del bitmap después de guardarlo para evitar reintroducir confianza en bytes que otro proceso pueda cambiar. El backend de prueba de fallo de sync necesitó conservar la identidad de su filesystem subyacente, porque la persistencia rechaza otros backends; la prueba final alcanza la barrera y confirma que no existe checkpoint. Se retiraron las interfaces internas de carga/borrado del fast resume que ya no se usan.

Validación: 42 pruebas de aplicación/integración pasan y 1 pública se ignora; 29 pruebas del motor pasan, 3 están ignoradas y 5 E2E se excluyen de esta ejecución (la suite de aplicación ya cubre transferencias loopback). Incluye modificaciones del mismo tamaño, sustitución, truncado, corrupción de la pieza 100, bitmap truncado, salida de proceso sin destructores, recheck manual y fallo inyectado de sync. Prueba de checkpoint: sustitución de longitud, error al publicar, limpieza de temporales y ausencia de flush en Drop. Build TypeScript/Vite y Clippy del motor con `-D warnings` pasan. No se simula una pérdida física de energía; la durabilidad se basa en las barreras del sistema operativo.

Los tres crates vendorizados son miembros del workspace, con la aplicación como miembro predeterminado, para ejecutar sus pruebas explícitamente. El lock incorpora dependencias opcionales/de prueba; no cambia ninguna versión previamente fijada ni activa esas capacidades en el binario de TorrentCat. Los comandos del motor usan `--no-default-features --features rust-tls`.

## F5: subida acotada y cancelable

Cada peer admite 32 solicitudes distintas, con un presupuesto compartido de 1.024 por sesión. Los permisos cubren espera de velocidad, cola y escritura; cancelar no libera capacidad mientras siga existiendo el trabajo. Los duplicados no crean trabajo adicional. Cada writer tiene además 256 mensajes como máximo; desbordarlo devuelve error en vez de crecer indefinidamente. El scheduler de cada torrent reparte un bloque por peer listo y vuelta. No se afirma equidad entre torrents ni una cota sobre el RSS total.

Las conexiones empiezan choked, admiten subida al recibir Interested y vuelven a choked con NotInterested. Cancel y NotInterested descartan solicitudes que todavía no están escribiéndose. Una trama parcialmente escrita se termina para conservar el encuadre del protocolo. Se anuncia `reqq=32`. La ventana de metadata se limita a 64 solicitudes y avanza con las respuestas. Los límites de velocidad inferiores a un bloque permiten una ráfaga inicial de un bloque, manteniendo la tasa configurada después.

Self-review: la prueba de wire descubrió que el handshake entrante consultaba nuestras capacidades y registraba nuestro ID, en lugar del handshake recibido. Se corrigieron ambas referencias. La identidad de cada reserva evita que el Drop de una solicitud cancelada elimine una solicitud nueva del mismo bloque. Mantener el permiso hasta Drop evita que alternar Request/Cancel acumule nodos sin presupuesto. Se corrigió también la capacidad mínima del rate limiter, que antes podía terminar el scheduler con tasas menores que 16 KiB/s.

Validación: 35 pruebas del motor pasan, 3 ignoradas y 5 E2E excluidas; incluye 100.000 solicitudes duplicadas, consumidores lentos, límite global, cancelación/repetición y round robin. Metadata de 32 MiB inicia 64 peticiones y repone una por respuesta. Un peer TCP loopback comprueba bitfield sin Unchoke anticipado, Interested, payload, Cancel/NotInterested durante espera de velocidad y reanudación. Suite de aplicación y Clippy del motor con `-D warnings` pasan. Sin hallazgos bloqueantes restantes en el diff.

## F6: niveles y cambio privado con barrera

Se conserva `announce-list` desde el metainfo original, también al restaurar ese archivo. Su presencia excluye `announce`. Los niveles se recorren en orden, las URLs de cada nivel se mezclan al iniciar y la URL exitosa se promueve dentro de su nivel. Los públicos vuelven a probar desde el primer nivel en cada anuncio. Los privados conservan su tracker actual hasta que falla. Los trackers adicionales de aplicación solo son respaldos para torrents públicos.

Antes de anunciar a otro tracker privado, la corriente de descubrimiento drena las direcciones previas y solicita una barrera de desconexión. Cada generación de peers tiene cancelación y tareas registradas. La barrera cierra admisión, cancela conexiones, espera sus tareas y elimina estados anteriores; las colas y reintentos conservan su generación y no pueden entrar en la nueva. Las reservas de piezas se liberan al cerrar cada conexión. Si el cierre falla o excede 5 s, se mantiene deshabilitada la admisión y no se contacta al respaldo. Se envía stopped al tracker anterior antes del nuevo started. La resolución de magnets aún desconocidos mantiene la política conservadora: no cambia de tracker sin un torrent administrado sobre el que aplicar aislamiento.

Self-review: se separó la referencia del consumidor a estadísticas de la referencia al emisor, evitando que una referencia circular mantuviera abierta la corriente al terminar el worker. Se verificó el registro de tareas bajo el mismo lock que cierra admisión, y se añadieron generaciones a los reintentos y a los peers que esperan un permiso. El parser distingue la ausencia de `announce-list` de una lista presente vacía, sin volver a `announce`. El crate de tracker declara sus propias features Tokio de pruebas, para que sus pruebas HTTP funcionen independientemente del workspace.

Validación: 36 pruebas del motor pasan (3 ignoradas, 5 E2E excluidas), 12 de tracker pasan (1 pública ignorada) y la suite de aplicación/integración pasa. Casos: prioridad entre niveles, promoción dentro de un nivel, deduplicación, preservación del metainfo, rechazo de trackers globales privados, desconexión TCP anterior, descarte de peers pendientes con límite de una conexión, conexión al nuevo peer y ausencia de failback mientras el respaldo funciona. Una prueba con barrera detenida confirma cero anuncios al respaldo hasta liberarla; un error inyectado impide el cambio. Clippy con `-D warnings` y revisión del diff pasan. Referencias: [BEP 12](https://www.bittorrent.org/beps/bep_0012.html), [BEP 27](https://www.bittorrent.org/beps/bep_0027.html).

## F7: recuperación de UDP y URLData

Los intercambios UDP usan 15 × 2ⁿ segundos, con n de 0 a 8 y un presupuesto finito de nueve intentos. Antes de reenviar un announce se comprueba la edad del connection ID: a los 60 s se obtiene otro. Se prueban en turno hasta ocho direcciones DNS distintas, con DNS limitado a 5 s; las IP literales se usan directamente. Cada intercambio reserva un ID nuevo, por lo que respuestas tardías de intentos anteriores quedan descartadas. Una sesión cancelada libera sus reservas inmediatamente. El límite exterior de 10 s queda reservado a HTTP; UDP dispone del tiempo de su política de recuperación. Pause/remove/shutdown conservan sus plazos cortos para stopped, sin esperar todo ese presupuesto.

Los anuncios incluyen URLData de BEP 41: ruta + query codificadas tal como aparecen en la URL, fragmentadas en opciones de hasta 255 bytes y limitadas a 4 KiB en total. Nunca se reintenta retirando la autenticación. Los errores de tracker se ocultan en los diagnósticos. Un connect admite campos adicionales después del encabezado conocido.

Self-review: se distinguió un rechazo explícito del tracker de la pérdida de paquetes, evitando una ráfaga de nueve solicitudes ante un rechazo. Los errores de envío esperan antes de volver a probar. Se añadió cancelación al cliente UDP y se corrigió el lookup de URLs IPv6 con corchetes. Los límites DNS y URLData acotan el trabajo por solicitud. El coste de la política completa es que un tracker silencioso puede tardar mucho en declararse fallido; las acciones del usuario siguen siendo cancelables.

Validación: pruebas con reloj Tokio simulado comprueban instantes exactos, pérdida de connect/announce, renovación a los 60 s, agotamiento de los nueve intentos, fallback IPv6→IPv4, respuestas IPv6, cancelación y rechazo sin ráfagas. URLData cubre 0/1/254/255/256/510/4.096 bytes y rechazo de 4.097; el tracker UDP local verifica la ruta/query original en anuncios reales. 19 pruebas del crate de tracker pasan (1 pública ignorada); 11 pruebas de integración de trackers pasan. Clippy con `-D warnings` pasa. Referencias: [BEP 15](https://www.bittorrent.org/beps/bep_0015.html) y [BEP 41](https://www.bittorrent.org/beps/bep_0041.html), cuya extensión sigue publicada como Draft.

## F8: negociación incremental y PEX

El mapa de extensiones es opcional y sus actualizaciones son incrementales: campos ausentes conservan su ID, cero desactiva y IDs activos duplicados se rechazan sin aplicar parcialmente el cambio. El writer toma un snapshot del mapa al serializar y descarta payloads pendientes de una extensión desactivada. El serializador también rechaza cero, reservado al handshake. Estas reglas se aplican en la copia vendorizada de `librqbit-peer-protocol` 9.0.1, del mismo commit upstream que los otros componentes; no se cambian versiones del lock. Se omite únicamente el benchmark y su dependencia Criterion.

PEX mantiene un solo worker por conexión. Activar, repetir, desactivar y reactivar modifica su estado sin duplicar tareas ni reiniciar su intervalo. Los ticks perdidos no se recuperan en ráfaga. Los contactos locales se filtran antes del límite de 50 y antes de crear el payload enviado a un peer público; también se excluyen direcciones inválidas, puerto cero y el propio destinatario. Se normalizan IPv4 mapeadas en IPv6 al clasificar su alcance. La vista del destinatario solo incorpora contactos que el writer confirma como escritos; el intervalo empieza después de esa confirmación, por lo que una cola lenta no genera mensajes seguidos.

Self-review: se corrigió el mapa `m` obligatorio del parser y la emisión potencial de payloads como handshake con ID cero. También se detectaron ticks acumulados de Tokio, confirmaciones prematuras al encolar PEX y el caso de multicast/unspecified IPv4 mapeado. Se sustituyó el intervalo por una espera después de cada confirmación del writer. La prueba con peers quietos detectó además un timeout de lectura de 10 s frente a keepalives de 120 s; las conexiones establecidas admiten ahora 180 s de inactividad por defecto, manteniendo 10 s para handshake/escritura. El código de lectura de metadata conserva mejoras de secuenciación pendientes de F14; esta etapa no declara resuelto ese hallazgo.

Validación: 24 pruebas del protocolo y 38 del motor pasan (3 del motor ignoradas y 5 E2E excluidas). Una conexión TCP real sirve metadata con IDs negociados, conserva el ID ante un delta de PEX y un handshake sin `m`, desactiva sin emitir payload y reactiva sobre la misma conexión. Una prueba TCP adicional envía 100 handshakes PEX y observa un solo mensaje con el contacto conectado. PEX cubre además 1.000 handshakes repetidos, disable/re-enable y decodificación del payload con contactos IPv4/IPv6, locales y mapeados. Suite de aplicación/integración y Clippy con `-D warnings` pasan. Referencias: [BEP 10](https://www.bittorrent.org/beps/bep_0010.html), [BEP 11](https://www.bittorrent.org/beps/bep_0011.html).


## F9: operaciones durables y cierre supervisado

La pausa persiste su intención antes de esperar anuncios finales. Borrar registra primero una intención durable, detiene transferencias, elimina los archivos y finalmente retira la entrada. Los errores de almacenamiento se devuelven y mantienen una operación reintentable; los de tracker no revierten el resultado local. Un borrado pendiente impide reanudar y se retoma al arrancar. Los reintentos no degradan “borrar datos” a “solo olvidar”, y borrar una entrada ya ausente es idempotente. Los IDs JSON conservan un contador durable para no reutilizar IDs borrados.

La base JSON y los archivos `.torrent` se reemplazan mediante temporal, fsync, rename y fsync del directorio. La copia en memoria solo cambia tras publicar; un error posterior a rename conserva el estado publicado pero informa que no se pudo confirmar durabilidad. Borrar sincroniza el directorio del archivo antes de retirar la intención. El backend PostgreSQL opcional incorpora la marca de borrado; se comprobó que compila, sin ejecutar un servidor PostgreSQL.

Cada torrent serializa sus operaciones; el lock global queda reservado al alta y al cierre de admisión, sin esperas de peers/trackers. Las altas del mismo hash comparten un lock durante resolución y publicación. Se bloquea el handle antes de publicarlo para que una pausa/eliminación temprana no se pierda. El cierre espera tareas propias de sesión, inicialización, transferencia, PEX, receptor UDP, observador de carpeta y notificaciones; desaparece el sleep fijo. Los anuncios finales tienen un presupuesto conjunto de 6 s, además de hasta 2 s para transferencias. El servidor de medios conserva y cancela cada conexión, también lectores detenidos, y espera su terminación.

Self-review y correcciones: liberar el permiso de trabajo bloqueante antes de guardar evitó una espera circular con un solo permiso; la prueba usa precisamente ese límite. Se mantuvo el descriptor confinado durante reintentos de borrado. Un fallo tras publicar la retirada del JSON ya no impide el siguiente reintento. Se impide que una pausa posterior resucite una entrada pendiente. Se corrigió la carrera al terminar el chequeo inicial consultando la intención actual; F12 conserva la revisión y regresiones deterministas específicas de inicialización. F11 conserva los casos de metadata ausente/corrupta y su presentación como recuperación, todavía pendientes. DHT/LSD externos reciben cancelación mediante su API, que no expone un handle de join; no se afirma haber añadido supervisión interna a esos crates.

Validación: 53 pruebas de aplicación e integración pasan (1 de red pública ignorada); 39 del motor pasan (3 ignoradas y 5 E2E excluidas); 19 de tracker pasan (1 pública ignorada). Incluyen error de publicación real, inyección de disco lleno y error posterior a publicación, reintento de pausa/resume/borrado, salida abrupta de un subproceso con borrado pendiente, reinicio sin reutilizar IDs, tracker silencioso sin lock global, límite total con tres trackers fallidos, dos altas del mismo magnet, tareas canceladas antes del retorno y conexiones HTTP activas/ociosas cerradas. Clippy del motor/tracker con `-D warnings` pasa. Todo usa fixtures temporales y red loopback; no se simuló pérdida física de energía ni se usó TD.


## F10: contabilidad de escrituras parciales

La subida contabiliza cada porción de payload que `AsyncWrite` acepta, inmediatamente después de cada escritura y antes del siguiente punto de cancelación. Excluye el preámbulo de pieza y los mensajes de control/metadata/PEX. Un error, una escritura de cero bytes, timeout o cancelación conserva el avance anterior. Esta frontera mide aceptación por el transporte: no afirma recepción remota ni cuenta retransmisiones TCP. Una nueva solicitud BitTorrent de un bloque ya enviado sí cuenta como otra transferencia. Los totales visibles usan el contador de la sesión y sobreviven a pausa/reanudación, como los anuncios.

Self-review: se comprobaron los cruces de cabecera/payload y la ausencia de await entre escribir y contabilizar. Se conservó la política de terminar una trama ya iniciada ante Cancel, cerrando la conexión ante error/timeout. La conversión del incremento a u32 es comprobada. La regresión de pausa detectó que el total visible se reiniciaba; ahora comparte el acumulador del tracker, sin alterar la base de los estimadores de velocidad.

Validación: 41 pruebas del motor pasan (3 ignoradas y 5 E2E excluidas), 54 de aplicación/integración pasan (1 pública ignorada), y Clippy del motor con `-D warnings` pasa. Un transporte controlado cubre cada posición del preámbulo, el primer byte de payload, bloque intermedio/último, éxito, error, WriteZero, timeout, cancelación y mensajes sin payload. El peer TCP repite una subida y confirma totales tras pausa/reanudación. Otro peer local envía primero una pieza corrupta y luego su reemplazo: `downloaded` pasa de 16.384 a 32.768; `left` solo baja de 16.384 a cero después del hash correcto, con `completed` y `stopped` finales coherentes. Las pruebas HTTP/UDP existentes confirman también los contadores de subida en `stopped`. No se usaron trackers externos.


## F11: recuperación visible sin resolución implícita

La ausencia, corrupción o sustitución del `.torrent` guardado deja una entrada de error en la biblioteca, conservando ID, nombre (o hash para perfiles antiguos), carpeta y selección. No se convierte a magnet ni impide abrir las entradas sanas. Los archivos internos se leen con límite de 32 MiB, verificación de tipo y, en Unix, apertura no bloqueante sin seguir el enlace final: ni un FIFO ni un enlace sustituido bloquean el arranque. El metainfo debe corresponder al hash persistido.

La tarjeta ofrece **Repair torrent…** para elegir el `.torrent` original. El backend rechaza otro hash antes de tocar datos, conserva ubicación/selección y vuelve a verificar los archivos en pausa. Antes de publicar metadata utilizable se persiste la pausa, de modo que un fallo intermedio no provoque actividad inesperada al reiniciar. Guardado atómico y errores propagados proceden de F9. Una intención pendiente de solo olvidar se completa sin necesitar metadata; una de borrar datos espera la reparación y luego termina el borrado. Los errores de eliminación pendientes son visibles. El backend PostgreSQL actualiza la metadata reparada sin perder la marca de eliminación.

Self-review: se eliminó toda resolución implícita, incluida la previsualización de un magnet cuyo hash ya está en recuperación. Se añadió validación del hash para impedir restaurar un `.torrent` intercambiado. Se comprobó el orden pausa durable → metadata → reemplazo del handle bajo locks, y la convivencia con cierre, altas duplicadas y borrado. La reparación mantiene el factory de almacenamiento configurado. El frontend no ofrece borrar datos de una entrada sin metadata, y los errores muestran una acción de reparación sin volcar contenido bencode o URLs del tracker.

Validación: 57 pruebas de aplicación/integración pasan (1 pública ignorada); 41 del motor pasan (3 ignoradas y 5 E2E excluidas). La matriz cubre ocho daños —ausente, vacío, inválido, hash distinto, tamaño excesivo, directorio, symlink y FIFO— junto a otra entrada sana, reinicio y reparación. Se verifican cero contactos al tracker loopback, rechazo de reparación incorrecta, contenido conservado, ID/carpeta/selección estables, falla real al publicar metadata, pausa persistida y convergencia de borrados pendientes. Una prueba de Core comprueba el snapshot que recibe la biblioteca y la reparación con el archivo seleccionado. Compilación TypeScript/Vite, Clippy con `-D warnings` y compilación del backend PostgreSQL opcional pasan; este último no se probó contra un servidor. No se usó TD.


## F12: cancelación e intención durante inicialización

La pausa cancela la espera del permiso y los puntos asíncronos del chequeo, y libera inmediatamente el stream de descubrimiento. El trabajo de disco síncrono termina dentro de su tarea antes de liberar su registro. Un guard restablece la propiedad del chequeo incluso si se cancela la sesión; reanudar puede iniciar otro chequeo. La transición exitosa consulta la intención actual de pausa/borrado bajo el lock de estado, incorporada en F9.

Self-review: se detectó además que reanudar una inicialización originalmente pausada perdía el nuevo stream de peers. Ahora el estado de inicialización conserva la actualización y la entrega al pasar a Live. Un segundo start no borra la cancelación del chequeo en curso. Se revisó el orden de locks, la liberación del guard antes del registro de tareas y la ausencia de trabajo de disco separado que pueda sobrevivir al borrado.

Validación: barreras deterministas retienen todos los permisos de inicialización; pausa, borrado y cierre terminan sin liberarlas y sin tareas de transferencia restantes. Tras liberar, el torrent borrado no reaparece y el pausado puede reanudar. Otra barrera comprueba que el stream actualizado se consume al pasar a Live. 43 pruebas del motor pasan (3 ignoradas, 5 E2E excluidas), la suite de aplicación/integración y Clippy con `-D warnings` pasan. Fixtures temporales sin descubrimiento externo.


## F13: límites al servir metadata

El servidor valida el índice antes de multiplicar o crear slices. Un índice inexistente recibe `reject` con el índice original, también para metadata vacía. El tamaño total se convierte con comprobación y el cálculo del rango no puede desbordarse. Las solicitudes válidas conservan su bloque y tamaño.

Self-review: además de corregir `>` por un límite exclusivo, se eliminó la multiplicación u32 vulnerable a desbordamiento y se mantuvo viva la conexión después del rechazo. La política privada existente no publica metadata. Validación: matriz de tamaños 0/1/16.383/16.384/16.385/32.768 y último/primer índice inválido/extremo; un peer TCP solicita 1 y u32::MAX sobre metadata de una pieza, recibe rejects y vuelve a obtener la pieza cero por la misma conexión. 44 pruebas del motor pasan (3 ignoradas y 5 E2E excluidas), la prueba TCP y Clippy con `-D warnings` pasan.


## F14: secuencia de negociación de metadata

El lector mantiene capacidad, tamaño, descarga y resultado entregado como estado explícito. Un `Data` antes de negociar produce un error del peer, sin unwrap; el resolver continúa con otros peers. Los campos de handshake pueden llegar en actualizaciones separadas. Repetirlos no reinicia piezas ni solicitudes; cero desactiva la capacidad y descarta ese peer. Se rechazan tamaño cero/excesivo/cambiante, total_size inconsistente, piezas no solicitadas y duplicadas. Las peticiones recibidas mientras falta metadata verificada reciben reject; no se envían Unchoke/Interested innecesarios para obtener metadata.

Self-review: se conserva una única ventana de 64 solicitudes, y el hash completo sigue siendo obligatorio antes de entregar el resultado. El tamaño declarado en cada bloque se contrasta con el negociado. Los errores de parseo no vuelcan el contenido en la nueva ruta. Validación: 46 pruebas del motor pasan (3 ignoradas, 5 E2E excluidas), suite de aplicación/integración y Clippy con `-D warnings` pasan. Un test usa el resolver real con dos peers TCP: el primero envía Data antes del handshake extendido; se espera su desconexión antes de que el segundo entregue metadata válida y complete la misma resolución. Pruebas adicionales cubren campos separados, 100 handshakes repetidos, desactivación, tamaños y duplicados. Referencia: [BEP 9](https://www.bittorrent.org/beps/bep_0009.html).


## F15: mapas de piezas dimensionados y límites por mensaje

La escritura reserva el bitfield según el número validado de piezas y libera ese buffer después de enviarlo. La lectura conserva un ring buffer que crece únicamente tras validar tipo y longitud del encabezado. Para torrents conocidos, el tamaño debe coincidir exactamente; durante resolución desconocida existe un máximo de 209.716 bytes, derivado del límite de metainfo de 32 MiB y los hashes SHA-1. Los bloques conservan 16 KiB de payload; las extensiones tienen un límite independiente de 32 KiB por trama y los controles no amplían el buffer.

Self-review: el control de tamaño ocurre después de cada lectura, evitando que una cabecera fragmentada ordene leer todo su tamaño antes de validarla. El crecimiento conserva las dos partes del ring; los diagnósticos usan usize para no truncar los tamaños nuevos. La trama que sigue a un bitfield se conserva. No se aumentó indiscriminadamente el buffer fijo de bloques.

Validación: 48 pruebas del motor pasan (3 ignoradas, 5 E2E excluidas); suite de aplicación/integración y Clippy con `-D warnings` pasan. Un peer TCP intercambia bitfields de 16.495/16.500/32.763/32.768/32.769 bytes; descarga la pieza que falta, verifica SHA-1/contenido y recibe una subida por la misma conexión. Son fixtures de piezas pequeñas para acotar disco, sin fingir solo la serialización. Otra prueba entrega la trama byte por byte con la cabecera cruzando el final del ring, seguida de Unchoke. Tamaños extremos/control/bloque/extensión, diferencia con metainfo y máximo absoluto se rechazan antes de crecer. Referencia: [BEP 3](https://www.bittorrent.org/beps/bep_0003.html).


## Verificación conjunta y límites

Sobre `d0a2899`: 58 pruebas de aplicación/integración, 48 del motor, 21 de bencode, 24 del protocolo y 19 de tracker pasan: **170 en total**. Hay 5 pruebas ignoradas y 5 E2E del motor excluidas; no se presentan como ejecutadas. La compilación TypeScript/Vite, Clippy del motor/protocolo con `-D warnings` y la compilación opcional PostgreSQL pasan. El PostgreSQL real, Miri, la pérdida física de energía y la interfaz nativa manual no se validaron. Las pruebas nuevas usan directorios temporales y loopback, sin TD ni cuentas privadas.

Las correcciones F1–F15 cierran hallazgos concretos, no constituyen una certificación completa de los BEP. El siguiente trabajo de calidad, en orden, es:

1. Ejecutar estas suites en CI macOS, incluyendo explícitamente crates vendorizados; añadir análisis de dependencias RustSec/npm y conservar resultados por commit.
2. Mantener corpus y fuzzing de parsers/negociación; probar fallos de disco, red y cancelación de manera continua.
3. Validar un swarm local con Transmission y libtorrent/qBittorrent, sesiones prolongadas IPv4/IPv6, sueño/reactivación y streaming con selección parcial.
4. Hacer visibles diagnósticos de tracker sin credenciales, documentar presupuestos de recursos y automatizar firma/notarización para distribución.

Las capacidades opcionales —v2/híbridos, web seeds o cambios en descubrimiento de magnets— requieren decisiones propias y no se declaran implementadas aquí.


## Revisión independiente de Claude CLI

Claude CLI revisó el código en modo de solo lectura sobre `d0a2899` (53 turnos, sin ejecutar pruebas). No señaló defectos en F13–F15; propuso seis casos adicionales. Sus afirmaciones se contrastan con código y regresiones antes de adoptarlas.

### C4: reanudar tras timeout de pausa — confirmado y corregido

Una pausa puede agotar 10 s mientras trabajo síncrono conserva la inicialización. Reanudar ahora espera la salida de esa generación cancelada antes de publicar intención activa o crear descubrimiento. Si sigue detenida, devuelve error y conserva la pausa; no informa una reanudación que terminará sin worker. Self-review: se comprueba de nuevo el cierre de sesión después de esperar. La regresión usa reloj simulado y conserva explícitamente guard/registro de tarea más allá del plazo; resume permanece pendiente hasta liberarlos y entonces llega a Live. Pasa junto con Clippy y la suite del motor.


### C1: UDP silencioso delante de respaldos — confirmado y corregido

Con varias URLs, cada candidato UDP dispone de 45 s (reintentos internos de 15/30 s) antes de pasar al siguiente. Un UDP fallido entra en cooldown progresivo de 60 s a una hora; no bloquea cada nuevo intervalo del respaldo. Un único tracker mantiene la recuperación completa de F7. Self-review: el cooldown se limita a este caso UDP, conservando el orden y la recuperación HTTP existente. La afirmación de Claude sobre `stopped` se descartó: cancelación y presupuesto final independiente ya lo acotaban.

Una regresión con reloj simulado, UDP silencioso en primer nivel y HTTP funcional en segundo confirma acceso al respaldo tras 45 s y anuncios posteriores sin volver a bloquearse en el primario. Pasan las 20 pruebas de tracker (1 pública ignorada) y Clippy con `-D warnings`.


### C2: fallback de magnets sin peers previos — confirmado y corregido

La resolución conserva el orden de `tr` y puede pasar al siguiente tracker mientras ningún tracker haya publicado un peer. Sigue finalizando el tracker previo antes de cambiar. Una vez encolado el primer peer, exige la barrera de aislamiento anterior; sin torrent administrado, falla cerrada. No se habilitan DHT/LSD/PEX, trackers globales ni peers iniciales para hashes desconocidos. Self-review: se separó el permiso de omitir una desconexión vacía de la obligación de finalizar el tracker anterior.

La regresión local completa un magnet con primer tracker HTTP 503 y segundo conectado a un seeder; comprueba el orden. El caso negativo recibe un peer, falla después el tracker y confirma cero contactos al respaldo. Pasan esa prueba, las 20 de tracker (1 ignorada) y Clippy con `-D warnings`.
