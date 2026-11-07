# kotlinx.serialization: keep generated serializers of @Serializable DTOs.
-keepattributes *Annotation*, InnerClasses
-keepclassmembers @kotlinx.serialization.Serializable class dev.sail.app.** {
    *** Companion;
    kotlinx.serialization.KSerializer serializer(...);
}
-keep,includedescriptorclasses class dev.sail.app.**$$serializer { *; }
