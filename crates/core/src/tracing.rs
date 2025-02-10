#[macro_export]
macro_rules! trace_call_result {
    ( expr: $call:expr, function_name: $function_name: expr ) => {{
        const FUNCTION_NAME: &'static str = $function_name;
        let span = ::tracing::trace_span!(FUNCTION_NAME);
        let _guard = span.enter();
        match $call {
            ::std::result::Result::Ok(x) => ::std::result::Result::Ok(x),
            ::std::result::Result::Err(err) => {
                let err_ref: &(dyn ::std::error::Error + 'static) = &err;
                ::tracing::error!(error = err_ref, "{} failed: {}", FUNCTION_NAME, err );
                ::std::result::Result::Err(err)
            }
        }
    }};
    ( $receiver:tt . $i:ident ( $( $arg:expr ),* $(,)? ) ) => {
        trace_call_result!( expr: $receiver . $i ( $( $arg, )* ) , function_name: stringify!($i) )
    };
    ( $i:ident $( :: $sub_i:ident )* ( $( $arg:expr ),* $(,)? ) ) => {{
        trace_call_result!( expr: $i$(::$sub_i)* ( $( $arg, )* ) , function_name: concat!(stringify!($i), $("::", stringify!($sub_i), )* )  )
    }};
}

/// Upcasts an error to `dyn Error + 'static`, which is useful when trying to use it as a field in a trace event.
#[cfg(feature = "std")]
pub fn error_value<T: std::error::Error + 'static>(e: &T) -> &(dyn std::error::Error + 'static) {
    e
}
