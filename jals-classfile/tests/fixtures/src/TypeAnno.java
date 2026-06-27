import java.lang.annotation.ElementType;
import java.lang.annotation.Retention;
import java.lang.annotation.RetentionPolicy;
import java.lang.annotation.Target;
import java.util.List;

public class TypeAnno {
    @Target(ElementType.TYPE_USE)
    @Retention(RetentionPolicy.RUNTIME)
    @interface RNonNull {}

    @Target(ElementType.TYPE_USE)
    @Retention(RetentionPolicy.CLASS)
    @interface CNonNull {}

    List<@RNonNull String> visible;
    List<@CNonNull String> invisible;

    @RNonNull
    String make(@CNonNull String p) {
        return p;
    }
}
