# Tareas de seguridad

Auditoría base: 2026-08-08. El rate limit ya está implementado; los siguientes puntos siguen pendientes.

## Trabajo urgente de despliegue

- [ ] Rotar la contraseña de MongoDB, `ADMIN_PASSWORD` y `AUDIT_LOG_ENCRYPTION_KEY` que fueron expuestas anteriormente. La rotación de la clave de auditoría debe planificarse porque los registros antiguos no podrán descifrarse con una clave nueva.
- [ ] Reemplazar la cuenta `root` de MongoDB por un usuario de privilegios mínimos limitado a `gym_tracker`.
- [ ] Colocar MongoDB detrás de una red privada o una allowlist de firewall y exigir TLS en `MONGODB_URI`.
- [ ] Impedir el acceso directo al origen de Coolify antes de depender de `TRUST_PROXY_HEADERS=true`.

## Confidencialidad y disponibilidad del registro de auditoría

- [ ] Sustituir las escrituras de auditoría ilimitadas y desacopladas por una cola acotada con métricas observables de fallos.

## Autenticación y sesiones

- [ ] Aumentar el mínimo de las contraseñas de usuarios y administradores y rechazar contraseñas débiles o conocidas como comprometidas.
- [ ] Añadir MFA o autenticación reforzada antes de mostrar registros de auditoría descifrados.
- [ ] Limitar las sesiones simultáneas y ofrecer una operación para revocarlas todas.

## Transporte, criptografía y retención

- [ ] Establecer TLS mínimo 1.2 o 1.3 en Cloudflare y habilitar HSTS.
- [ ] Derivar claves independientes para AES e índices ciegos mediante HKDF y añadir versiones explícitas de claves para permitir rotaciones.
- [ ] Tratar `x-gym-client` y las IP reenviadas como metadatos forenses no confiables, salvo que hayan sido verificados por un proxy confiable.

## Garantías y cadena de suministro

- [ ] Migrar el rate limit a un almacén compartido como Redis si la API se escala a varias réplicas; el contador actual es local a cada instancia y se reinicia con el proceso.
- [ ] Añadir pruebas de integración para el bloqueo de login, CSRF, autorización administrativa, aislamiento entre usuarios, revocación de sesiones y acceso a auditorías.
- [ ] Añadir `cargo audit` y Clippy estricto al CI.
- [ ] Escanear la imagen final del contenedor, fijar los digests de las imágenes base y generar un SBOM.

## Completado

- [x] Añadir límites por cliente, por intento de login y por instancia, con respuestas `429` y `Retry-After`; las peticiones bloqueadas también quedan registradas por la auditoría existente.
- [x] El límite por sesión solo se aplica a sesiones que existen en la base de datos; las cookies inventadas caen en el límite por IP.
- [x] Auditoría: se censuran contraseñas, tokens, `Cookie`, `Authorization` y `Set-Cookie` (de las cookies queda una huella SHA-256 corta); del cuerpo se guarda una vista previa de 4.096 caracteres, su longitud y su SHA-256; los cuerpos de más de 2 MiB se rechazan con `413`.
- [x] Las respuestas de auditoría descifradas llevan `Cache-Control: no-store`, y la web no guarda `/admin` en el service worker.
- [x] El login verifica un hash Argon2 ficticio cuando el usuario no existe.
- [x] Los tokens de sesión se guardan como SHA-256 (`tokenHashed: true`); las sesiones antiguas se migran al arrancar sin cerrar la sesión de nadie.
- [x] Las sesiones caducan tras 60 días sin uso (antes, 365 días fijos); cada uso las renueva, como mucho una vez al día.
- [x] `sync_mutations` tiene TTL de 180 días y se borra al eliminar un usuario.
- [x] Los errores 500 se escriben en stderr (logs de Coolify).
