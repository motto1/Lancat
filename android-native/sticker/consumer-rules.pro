# JNI：native 方法按名字绑定（Java_fan_lancat_sticker_LottieNative_*）
-keepclasseswithmembernames,includedescriptorclasses class fan.lancat.sticker.LottieNative {
    native <methods>;
}
-keep class fan.lancat.sticker.LottieNative { *; }
