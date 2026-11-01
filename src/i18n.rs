//! Idioma de la CLI. Español por defecto. Con `DOCKYARD_LANG=en` (o `LANG=en_*`) sale en inglés.
//!
//! Las cadenas van en pares: `crate::tr!("español {x}", "english {x}", x)`. Es deliberadamente simple: sin tablas
//! externas ni dependencias; el par está junto al texto, así que no se olvida una traducción sin notarlo.

use std::sync::OnceLock;

/// ¿Mensajes en inglés? Se decide una vez por proceso.
pub fn english() -> bool {
    static EN: OnceLock<bool> = OnceLock::new();
    *EN.get_or_init(|| {
        let v = std::env::var("DOCKYARD_LANG").or_else(|_| std::env::var("LANG")).unwrap_or_default();
        v.to_lowercase().starts_with("en")
    })
}

/// Elige la versión en inglés o en español y la formatea con los argumentos dados.
#[macro_export]
macro_rules! tr {
    ($es:literal, $en:literal $(, $arg:expr)* $(,)?) => {
        if $crate::i18n::english() { format!($en $(, $arg)*) } else { format!($es $(, $arg)*) }
    };
}

#[cfg(test)]
mod tests {
    #[test]
    fn picks_the_language_and_formats_arguments() {
        // el idioma depende del entorno; se comprueba que ambas ramas formatean igual
        let n = 3;
        let s = crate::tr!("{n} contenedores", "{n} containers");
        assert!(s == "3 contenedores" || s == "3 containers", "{s}");
        let t = crate::tr!("{} de {}", "{} of {}", 1, 2);
        assert!(t == "1 de 2" || t == "1 of 2", "{t}");
    }
}
