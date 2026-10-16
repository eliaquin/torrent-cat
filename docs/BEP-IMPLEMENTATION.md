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
| F5 — capacidad, cancelación y equidad de subida | Implementado, revisado y validado | Colas acotadas y peer loopback |
| F6 — niveles y failover de trackers | Pendiente | |
| F7 — recuperación y URLData UDP | Pendiente | |
| F8 — extensiones y PEX | Pendiente | |
| F9 — ciclo de vida y persistencia | Pendiente | |
| F10 — contabilidad de escrituras parciales | Pendiente | |
| F11 — recuperación de metadata persistida | Pendiente | |
| F12 — pausa y borrado durante inicialización | Pendiente | |
| F13 — límites al servir metadata | Pendiente | |
| F14 — secuencia al recibir metadata | Pendiente | |
| F15 — buffers para mapas de piezas | Pendiente | |

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
