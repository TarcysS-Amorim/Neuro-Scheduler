//! App em foco, lido pelo socket IPC do compositor (Hyprland ou niri).
//!
//! Como qualquer outra leitura do coletor, isto é só consulta: nenhum comando
//! que altere o compositor é enviado.
//!
//! O coletor roda como serviço do systemd, pode subir antes do compositor e
//! **não** herda as variáveis da sessão gráfica. Por isso o socket é sempre
//! procurado varrendo o `XDG_RUNTIME_DIR`; `HYPRLAND_INSTANCE_SIGNATURE` e
//! `NIRI_SOCKET` ficam só como último recurso. O endpoint que responde fica em
//! cache e é redescoberto a cada falha, o que cobre o compositor subindo
//! depois, reiniciando (a assinatura do Hyprland muda a cada login) ou sendo
//! trocado por outro sem o serviço reiniciar.

use std::collections::HashSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::MetadataExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

/// Teto para o IPC não segurar o ciclo de amostragem se o compositor travar.
const IO_TIMEOUT: Duration = Duration::from_millis(200);

/// Resposta maior que isto é descartada: as duas respostas esperadas têm menos
/// de 2 KiB, e um título de janela gigante não deve virar alocação sem limite.
const MAX_REPLY: u64 = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Hyprland,
    Niri,
}

/// App em foco em uma amostra. Tudo opcional: pode não haver compositor, ou
/// haver compositor sem nenhuma janela focada.
#[derive(Debug, Default, Clone)]
pub struct FocusInfo {
    /// `class` no Hyprland, `app_id` no niri.
    pub app: Option<String>,
    pub pid: Option<i32>,
}

pub struct Focus {
    /// Último endpoint que respondeu.
    endpoint: Option<(Kind, PathBuf)>,
}

impl Focus {
    pub fn new() -> Self {
        Self { endpoint: None }
    }

    /// Consulta o compositor. Nunca falha: sem compositor acessível devolve
    /// campos vazios, que viram NULL no banco.
    pub fn current(&mut self) -> FocusInfo {
        if let Some((kind, path)) = self.endpoint.clone() {
            match query(kind, &path) {
                Ok(info) => return info,
                // Socket morreu (compositor caiu ou trocou de assinatura):
                // descarta o cache e tenta descobrir de novo agora mesmo.
                Err(()) => self.endpoint = None,
            }
        }

        for (kind, path) in candidates() {
            if let Ok(info) = query(kind, &path) {
                self.endpoint = Some((kind, path));
                return info;
            }
        }

        FocusInfo::default()
    }
}

/// `Err(())` só quando o socket não respondeu. Compositor vivo sem janela
/// focada é `Ok` com campos vazios — isso não invalida o endpoint em cache.
fn query(kind: Kind, path: &Path) -> Result<FocusInfo, ()> {
    let reply = request(kind, path)?;

    let (app_key, pid_key) = match kind {
        Kind::Hyprland => ("class", "pid"),
        Kind::Niri => ("app_id", "pid"),
    };

    Ok(FocusInfo {
        app: json::string(&reply, app_key).filter(|s| !s.is_empty()),
        pid: json::int(&reply, pid_key).filter(|p| *p > 0),
    })
}

fn request(kind: Kind, path: &Path) -> Result<String, ()> {
    let stream = UnixStream::connect(path).map_err(|_| ())?;
    stream.set_read_timeout(Some(IO_TIMEOUT)).map_err(|_| ())?;
    stream.set_write_timeout(Some(IO_TIMEOUT)).map_err(|_| ())?;

    let mut stream = stream;
    match kind {
        Kind::Hyprland => {
            stream.write_all(b"j/activewindow").map_err(|_| ())?;
            stream.flush().map_err(|_| ())?;
            // O Hyprland responde e fecha, então dá para ler até o fim.
            let mut reply = String::new();
            stream
                .take(MAX_REPLY)
                .read_to_string(&mut reply)
                .map_err(|_| ())?;
            (!reply.is_empty()).then_some(reply).ok_or(())
        }
        Kind::Niri => {
            stream.write_all(b"\"FocusedWindow\"\n").map_err(|_| ())?;
            stream.flush().map_err(|_| ())?;
            // O niri mantém a conexão aberta depois de responder (ela serve
            // para streams de eventos), então lê-se exatamente uma linha.
            let mut reply = String::new();
            BufReader::new(stream.take(MAX_REPLY))
                .read_line(&mut reply)
                .map_err(|_| ())?;
            (!reply.is_empty()).then_some(reply).ok_or(())
        }
    }
}

/// Sockets a tentar, em ordem de preferência.
///
/// A varredura do runtime dir vem primeiro, e as variáveis de ambiente só
/// depois: o coletor sobe pelo systemd e pode subir **antes** do compositor,
/// então o ambiente da partida não descreve a sessão que está viva agora. Uma
/// variável herdada de um login anterior aponta para um socket morto, e
/// confiar nela primeiro só adiaria a descoberta do socket certo. Varrendo o
/// diretório a cada falha, trocar de compositor também é resolvido sozinho.
fn candidates() -> Vec<(Kind, PathBuf)> {
    let runtime = runtime_dir();
    let mut out: Vec<(Kind, PathBuf)> = Vec::new();

    // 1. Varredura do runtime dir. Instâncias antigas do Hyprland deixam o
    //    diretório para trás sem o socket, então a ordem por mtime só desempata
    //    entre instâncias realmente vivas.
    if let Some(rt) = &runtime {
        let mut hypr: Vec<PathBuf> = entries(&rt.join("hypr"))
            .into_iter()
            .map(|dir| dir.join(".socket.sock"))
            .filter(|p| p.exists())
            .collect();
        hypr.sort_by_key(|p| std::cmp::Reverse(mtime(p)));
        out.extend(hypr.into_iter().map(|p| (Kind::Hyprland, p)));

        let mut niri: Vec<PathBuf> = entries(rt)
            .into_iter()
            .filter(|p| {
                p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("niri.") && n.ends_with(".sock"))
            })
            .collect();
        niri.sort_by_key(|p| std::cmp::Reverse(mtime(p)));
        out.extend(niri.into_iter().map(|p| (Kind::Niri, p)));
    }

    // 2. Variáveis da sessão, como último recurso: cobrem um socket fora do
    //    runtime dir, que o `NIRI_SOCKET` aceita.
    if let (Some(his), Some(rt)) = (env_non_empty("HYPRLAND_INSTANCE_SIGNATURE"), &runtime) {
        out.push((
            Kind::Hyprland,
            rt.join("hypr").join(his).join(".socket.sock"),
        ));
    }
    if let Some(p) = env_non_empty("NIRI_SOCKET") {
        out.push((Kind::Niri, PathBuf::from(p)));
    }

    // `dedup` comum só tiraria repetições vizinhas, e aqui a mesma via pode
    // aparecer na varredura e de novo no fim, vinda do ambiente.
    let mut vistos = HashSet::new();
    out.retain(|(_, path)| vistos.insert(path.clone()));
    out
}

fn entries(dir: &Path) -> Vec<PathBuf> {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    rd.flatten().map(|e| e.path()).collect()
}

fn mtime(path: &Path) -> SystemTime {
    std::fs::metadata(path)
        .and_then(|m| m.modified())
        .unwrap_or(SystemTime::UNIX_EPOCH)
}

/// `XDG_RUNTIME_DIR`, ou `/run/user/<uid>` quando a variável não vem.
///
/// O serviço de usuário normalmente recebe a variável, mas o fallback existe
/// para que o coletor ache o compositor mesmo iniciado de um ambiente pelado
/// (um `systemd-run` à mão, por exemplo). O uid sai do dono de `/proc/self`,
/// que é o próprio processo, sem precisar de `libc`.
fn runtime_dir() -> Option<PathBuf> {
    if let Some(dir) = env_non_empty("XDG_RUNTIME_DIR") {
        return Some(PathBuf::from(dir));
    }
    let uid = std::fs::metadata("/proc/self").ok()?.uid();
    let dir = PathBuf::from(format!("/run/user/{uid}"));
    dir.is_dir().then_some(dir)
}

fn env_non_empty(var: &str) -> Option<String> {
    std::env::var(var).ok().filter(|v| !v.is_empty())
}

/// Extração dos poucos campos que interessam, sem dependência de JSON.
///
/// O scanner anda em modo token e consome cada string inteira, com escapes.
/// É isso que impede um título de janela de forjar um campo: em
/// `"title":"\",\"app_id\":\"falso"` o título é um único token, e o `app_id`
/// que ele contém nunca é lido como chave.
mod json {
    /// Byte logo depois de `"<key>":`. Casa a primeira ocorrência em qualquer
    /// profundidade, o que basta para as chaves usadas aqui (`class`, `app_id`
    /// e `pid` aparecem uma única vez nas duas respostas).
    fn after_key(src: &str, key: &str) -> Option<usize> {
        let b = src.as_bytes();
        let mut i = 0;
        while i < b.len() {
            if b[i] != b'"' {
                i += 1;
                continue;
            }
            let (token, end) = scan_string(b, i)?;
            // Em JSON válido só uma chave é seguida de ':', nunca um valor.
            let next = end
                + b[end..]
                    .iter()
                    .take_while(|c| c.is_ascii_whitespace())
                    .count();
            if b.get(next) == Some(&b':') && token == key {
                return Some(next + 1);
            }
            i = end;
        }
        None
    }

    pub fn string(src: &str, key: &str) -> Option<String> {
        let at = after_key(src, key)?;
        let b = src.as_bytes();
        let start = at
            + b[at..]
                .iter()
                .take_while(|c| c.is_ascii_whitespace())
                .count();
        if b.get(start) != Some(&b'"') {
            // `null` ou número no lugar de string: nada a registrar.
            return None;
        }
        scan_string(b, start).map(|(s, _)| s)
    }

    pub fn int(src: &str, key: &str) -> Option<i32> {
        let at = after_key(src, key)?;
        let rest = src[at..].trim_start();
        let end = rest
            .find(|c: char| !c.is_ascii_digit() && c != '-')
            .unwrap_or(rest.len());
        rest[..end].parse().ok()
    }

    /// Lê a string que começa em `start` (um `"`) e devolve o conteúdo já sem
    /// escapes, mais o índice logo após a aspa de fechamento.
    fn scan_string(b: &[u8], start: usize) -> Option<(String, usize)> {
        let mut out = String::new();
        let mut i = start + 1;
        while i < b.len() {
            match b[i] {
                b'"' => return Some((out, i + 1)),
                b'\\' => {
                    let esc = *b.get(i + 1)?;
                    i += 2;
                    match esc {
                        b'n' => out.push('\n'),
                        b't' => out.push('\t'),
                        b'r' => out.push('\r'),
                        b'b' => out.push('\u{8}'),
                        b'f' => out.push('\u{c}'),
                        b'u' => {
                            // Só o plano básico; par substituto vira U+FFFD.
                            let hex = b.get(i..i + 4).and_then(|h| std::str::from_utf8(h).ok());
                            let cp = hex.and_then(|h| u32::from_str_radix(h, 16).ok());
                            out.push(cp.and_then(char::from_u32).unwrap_or('\u{fffd}'));
                            i += 4;
                        }
                        other => out.push(other as char),
                    }
                }
                _ => {
                    // Fatia bruta até o próximo escape para não decodificar
                    // UTF-8 byte a byte (títulos acentuados são comuns).
                    let run = b[i..]
                        .iter()
                        .take_while(|c| **c != b'"' && **c != b'\\')
                        .count();
                    out.push_str(std::str::from_utf8(&b[i..i + run]).ok()?);
                    i += run;
                }
            }
        }
        None
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn le_campos_do_hyprland() {
            let src = r#"{"class": "kitty", "title": "~", "pid": 51951}"#;
            assert_eq!(string(src, "class").as_deref(), Some("kitty"));
            assert_eq!(int(src, "pid"), Some(51951));
        }

        #[test]
        fn le_campos_do_niri() {
            let src =
                r#"{"Ok":{"FocusedWindow":{"id":3,"title":"x","app_id":"firefox","pid":42}}}"#;
            assert_eq!(string(src, "app_id").as_deref(), Some("firefox"));
            assert_eq!(int(src, "pid"), Some(42));
        }

        #[test]
        fn titulo_nao_forja_campo() {
            // O título contém um `app_id` falso e um `pid` falso, escapados.
            let src = r#"{"title":"\",\"app_id\":\"falso\",\"pid\":\"1","app_id":"real","pid":7}"#;
            assert_eq!(string(src, "app_id").as_deref(), Some("real"));
            assert_eq!(int(src, "pid"), Some(7));
        }

        #[test]
        fn sem_janela_focada() {
            assert_eq!(string("{}", "class"), None);
            assert_eq!(string(r#"{"Ok":{"FocusedWindow":null}}"#, "app_id"), None);
            assert_eq!(int("{}", "pid"), None);
        }

        #[test]
        fn escapes_e_acentos() {
            let src = r#"{"class":"a\\bç\"c","title":"Ação — Brave"}"#;
            assert_eq!(string(src, "class").as_deref(), Some("a\\bç\"c"));
            assert_eq!(string(src, "title").as_deref(), Some("Ação — Brave"));
        }
    }
}
