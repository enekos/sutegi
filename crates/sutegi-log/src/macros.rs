#[doc(hidden)]
#[macro_export]
macro_rules! __fields {
    (@ [$($out:tt)*]) => { [$($out)*] };
    (@ [$($out:tt)*] $k:ident = % $v:expr $(, $($rest:tt)*)?) => {
        $crate::__fields!(@ [$($out)* (stringify!($k), $crate::Value::display(&$v)),] $($($rest)*)?)
    };
    (@ [$($out:tt)*] $k:ident = $v:expr $(, $($rest:tt)*)?) => {
        $crate::__fields!(@ [$($out)* (stringify!($k), $crate::ToValue::to_value(&$v)),] $($($rest)*)?)
    };
}

#[macro_export]
macro_rules! log {
    ($level:expr, $target:expr, $msg:expr $(,)?) => {{
        let level: $crate::Level = $level;
        let target: &str = $target;
        if $crate::enabled(level, target) {
            $crate::emit(level, target, ::core::convert::AsRef::<str>::as_ref(&$msg), &[]);
        }
    }};
    ($level:expr, $target:expr, $msg:expr, $($fields:tt)+) => {{
        let level: $crate::Level = $level;
        let target: &str = $target;
        if $crate::enabled(level, target) {
            $crate::emit(
                level,
                target,
                ::core::convert::AsRef::<str>::as_ref(&$msg),
                &$crate::__fields!(@ [] $($fields)+),
            );
        }
    }};
}

#[macro_export]
macro_rules! debug {
    ($($t:tt)+) => { $crate::log!($crate::Level::Debug, $($t)+) };
}

#[macro_export]
macro_rules! info {
    ($($t:tt)+) => { $crate::log!($crate::Level::Info, $($t)+) };
}

#[macro_export]
macro_rules! warn {
    ($($t:tt)+) => { $crate::log!($crate::Level::Warn, $($t)+) };
}

#[macro_export]
macro_rules! error {
    ($($t:tt)+) => { $crate::log!($crate::Level::Error, $($t)+) };
}
