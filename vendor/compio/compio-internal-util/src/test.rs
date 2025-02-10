#[macro_export]
macro_rules! assert_func_send {
    { $type_name:ident :: $method_name:ident (&mut self , $( $arg_name:ident : $arg_ty:ty ),* ) } => {
        #[allow(dead_code)]
        const _: fn($type_name , $(  $arg_name : $arg_ty , )*) = |mut instance , $(  $arg_name , )*| {
            fn is_send<T: Send>(_t: T) {}
            is_send($type_name :: $method_name (
                &mut instance,
                $( $arg_name , )*
            ));
        };
    };
    { $type_name:ident :: $method_name:ident ( $( $arg_name:ident : $arg_ty:ty ),* ) } => {
        #[allow(dead_code)]
        const _: fn($(  $arg_name : $arg_ty , )*) = | $( $arg_name ,  )* |  {
            fn is_send<T: Send>(_t: T) {}
            is_send($type_name :: $method_name (
                $( $arg_name , )*
            ));
        };
    };

    { $func_name:ident ($( $arg_name:ident : $arg_ty:ty ),* ) } => {
        #[allow(dead_code)]
        const _: fn($(  $arg_name : $arg_ty , )*) = |$(  $arg_name , )*| {
            fn is_send<T: Send>(_t: T) {}
            is_send($func_name (
                $( $arg_name , )*
            ));
        };
    };
}
