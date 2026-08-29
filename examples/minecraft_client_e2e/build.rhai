// Generated. Regenerate with `examples/scripts/gen-client-runtime.py 1.21.11`.
//
// Everything the vanilla launcher would assemble, declared instead: the libraries the client
// links against, the native `.so` files the JVM needs on its library path, and the slice of the
// asset store a rendered frame actually reads. The game jar itself is not here — it arrives
// through `[dependencies] minecraft`, already remapped to official names, which is what lets the
// driver below be written in Java rather than in reflection.

if !build.feature("1.21.11") {
    build.error("this example targets one release; run it with `--features 1.21.11`.");
}

// --- libraries -------------------------------------------------------------------------------
// The 52 entries the release metadata selects for linux/x86_64, minus the natives below.

tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/at/yawk/lz4/lz4-java/1.8.1/lz4-java-1.8.1.jar"),
    tasks.sha1("1f30a180ee04ccd53339a716aec13cae60a013b5"), tasks.bytes(720896)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/com/azure/azure-json/1.4.0/azure-json-1.4.0.jar"),
    tasks.sha1("fcc1d354dbc3e0300e5276b1bf124d0247799cd8"), tasks.bytes(327680)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/com/github/oshi/oshi-core/6.9.0/oshi-core-6.9.0.jar"),
    tasks.sha1("03224870731860cfcd7744581a05b559e94291e7"), tasks.bytes(1048576)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/com/google/code/gson/gson/2.13.2/gson-2.13.2.jar"),
    tasks.sha1("48b8230771e573b54ce6e867a9001e75977fe78e"), tasks.bytes(327680)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/com/google/guava/failureaccess/1.0.3/failureaccess-1.0.3.jar"),
    tasks.sha1("aeaffd00d57023a2c947393ed251f0354f0985fc"), tasks.bytes(65536)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/com/google/guava/guava/33.5.0-jre/guava-33.5.0-jre.jar"),
    tasks.sha1("8699de25f2f979108d6c1b804a7ba38cda1116bc"), tasks.bytes(3080192)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/com/ibm/icu/icu4j/77.1/icu4j-77.1.jar"),
    tasks.sha1("38693cf0b1d7362a8b726af74dc06026a7c23809"), tasks.bytes(14680064)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/com/microsoft/azure/msal4j/1.23.1/msal4j-1.23.1.jar"),
    tasks.sha1("6c722b514873b24a4e1ce9c22dca36ea3c22bdbe"), tasks.bytes(393216)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/com/mojang/authlib/7.0.61/authlib-7.0.61.jar"),
    tasks.sha1("efee1e6b54e863108576eb3b3ae71144626aaefc"), tasks.bytes(131072)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/com/mojang/blocklist/1.0.10/blocklist-1.0.10.jar"),
    tasks.sha1("5c685c5ffa94c4cd39496c7184c1d122e515ecef"), tasks.bytes(65536)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/com/mojang/brigadier/1.3.10/brigadier-1.3.10.jar"),
    tasks.sha1("d15b53a14cf20fdcaa98f731af5dda654452c010"), tasks.bytes(131072)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/com/mojang/datafixerupper/9.0.19/datafixerupper-9.0.19.jar"),
    tasks.sha1("4e91f9712fa1e83231d1501625381b0210a977da"), tasks.bytes(786432)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/com/mojang/jtracy/1.0.37/jtracy-1.0.37.jar"),
    tasks.sha1("a3fb649b2e2b2288d2978f0b0bc3d83f62809d1e"), tasks.bytes(65536)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/com/mojang/logging/1.6.11/logging-1.6.11.jar"),
    tasks.sha1("0fd147240733010c158249d986323f9ef98977fe"), tasks.bytes(65536)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/com/mojang/patchy/2.2.10/patchy-2.2.10.jar"),
    tasks.sha1("da05971b07cbb379d002cf7eaec6a2048211fefc"), tasks.bytes(65536)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/com/mojang/text2speech/1.18.11/text2speech-1.18.11.jar"),
    tasks.sha1("e853a12cdd6ba4f4836e8f4bf3b37844a13482b6"), tasks.bytes(65536)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/commons-codec/commons-codec/1.19.0/commons-codec-1.19.0.jar"),
    tasks.sha1("8c0dbe3ae883fceda9b50a6c76e745e548073388"), tasks.bytes(393216)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/commons-io/commons-io/2.20.0/commons-io-2.20.0.jar"),
    tasks.sha1("36f3474daec2849c149e877614e7f979b2082cd2"), tasks.bytes(589824)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/io/netty/netty-buffer/4.2.7.Final/netty-buffer-4.2.7.Final.jar"),
    tasks.sha1("5555ff561643bf2f8430fb57c24403c0efe15994"), tasks.bytes(393216)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/io/netty/netty-codec-base/4.2.7.Final/netty-codec-base-4.2.7.Final.jar"),
    tasks.sha1("915e381ebabcf115f1c7ff7032d55c48afb50210"), tasks.bytes(196608)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/io/netty/netty-codec-compression/4.2.7.Final/netty-codec-compression-4.2.7.Final.jar"),
    tasks.sha1("572341bc1ca90fd9d6e47f1d2694aab5258566e9"), tasks.bytes(196608)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/io/netty/netty-codec-http/4.2.7.Final/netty-codec-http-4.2.7.Final.jar"),
    tasks.sha1("b734c108854099c421fd94d92d9f865e4d4da853"), tasks.bytes(720896)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/io/netty/netty-common/4.2.7.Final/netty-common-4.2.7.Final.jar"),
    tasks.sha1("11aa30df26af4fca3239ac1917f303a280f301e1"), tasks.bytes(851968)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/io/netty/netty-handler/4.2.7.Final/netty-handler-4.2.7.Final.jar"),
    tasks.sha1("7ad8a1f851e2e6fe93cdd091871fda2b81c03b5b"), tasks.bytes(589824)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/io/netty/netty-resolver/4.2.7.Final/netty-resolver-4.2.7.Final.jar"),
    tasks.sha1("5f3e5ef8de03992cd4fb46960dc0085ec1a12a12"), tasks.bytes(65536)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/io/netty/netty-transport-classes-epoll/4.2.7.Final/netty-transport-classes-epoll-4.2.7.Final.jar"),
    tasks.sha1("1075c09f48a78eef9d819fbfd9096b903fdd362a"), tasks.bytes(196608)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/io/netty/netty-transport-classes-kqueue/4.2.7.Final/netty-transport-classes-kqueue-4.2.7.Final.jar"),
    tasks.sha1("bdec7c23c75caabd848426d073c09568e8d5f94e"), tasks.bytes(131072)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/io/netty/netty-transport-native-epoll/4.2.7.Final/netty-transport-native-epoll-4.2.7.Final-linux-aarch_64.jar"),
    tasks.sha1("98d02251c98c8f07a23ac5ea27657d9e0ef57614"), tasks.bytes(65536)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/io/netty/netty-transport-native-epoll/4.2.7.Final/netty-transport-native-epoll-4.2.7.Final-linux-x86_64.jar"),
    tasks.sha1("e83998bfc10b5289d9bcbe807c4079ef0eed8e6f"), tasks.bytes(65536)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/io/netty/netty-transport-native-unix-common/4.2.7.Final/netty-transport-native-unix-common-4.2.7.Final.jar"),
    tasks.sha1("89953f04259ea7502cffb313630dd51e00e60669"), tasks.bytes(65536)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/io/netty/netty-transport/4.2.7.Final/netty-transport-4.2.7.Final.jar"),
    tasks.sha1("83ea548981d0d8c4a98027cc1a6f9624f902e142"), tasks.bytes(589824)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/it/unimi/dsi/fastutil/8.5.18/fastutil-8.5.18.jar"),
    tasks.sha1("a6cff377eecc19c2037bf31568a6d7106b50ba1f"), tasks.bytes(23986176)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/net/java/dev/jna/jna-platform/5.17.0/jna-platform-5.17.0.jar"),
    tasks.sha1("a4934c44d25a9d8c2ddf4203affd20330cb3426f"), tasks.bytes(1441792)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/net/java/dev/jna/jna/5.17.0/jna-5.17.0.jar"),
    tasks.sha1("33d12735bef894440780fce64f9758d420c7bae2"), tasks.bytes(2031616)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/net/sf/jopt-simple/jopt-simple/5.0.4/jopt-simple-5.0.4.jar"),
    tasks.sha1("4fdac2fbe92dfad86aa6e9301736f6b4342a3f5c"), tasks.bytes(131072)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/org/apache/commons/commons-compress/1.28.0/commons-compress-1.28.0.jar"),
    tasks.sha1("e482f2c7a88dac3c497e96aa420b6a769f59c8d7"), tasks.bytes(1179648)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/org/apache/commons/commons-lang3/3.19.0/commons-lang3-3.19.0.jar"),
    tasks.sha1("d6524b169a6574cd253760c472d419b47bfd37e6"), tasks.bytes(720896)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/org/apache/logging/log4j/log4j-api/2.25.2/log4j-api-2.25.2.jar"),
    tasks.sha1("292c1a2b1702f1e1e3adb13e1c57e5bff60335ff"), tasks.bytes(393216)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/org/apache/logging/log4j/log4j-core/2.25.2/log4j-core-2.25.2.jar"),
    tasks.sha1("d4d0ad2e51e03e531f784891fbfff1bae1e13a12"), tasks.bytes(2031616)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/org/apache/logging/log4j/log4j-slf4j2-impl/2.25.2/log4j-slf4j2-impl-2.25.2.jar"),
    tasks.sha1("5eec0c392661dee8a366baec17e8896900dd978f"), tasks.bytes(65536)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/org/jcraft/jorbis/0.0.17/jorbis-0.0.17.jar"),
    tasks.sha1("8872d22b293e8f5d7d56ff92be966e6dc28ebdc6"), tasks.bytes(131072)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/org/joml/joml/1.10.8/joml-1.10.8.jar"),
    tasks.sha1("fc0a71dad90a2cf41d82a76156a0e700af8e4f8d"), tasks.bytes(786432)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/org/jspecify/jspecify/1.0.0/jspecify-1.0.0.jar"),
    tasks.sha1("7425a601c1c7ec76645a78d22b8c6a627edee507"), tasks.bytes(65536)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/org/lwjgl/lwjgl-freetype/3.3.3/lwjgl-freetype-3.3.3.jar"),
    tasks.sha1("a0db6c84a8becc8ca05f9dbfa985edc348a824c7"), tasks.bytes(458752)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/org/lwjgl/lwjgl-glfw/3.3.3/lwjgl-glfw-3.3.3.jar"),
    tasks.sha1("efa1eb78c5ccd840e9f329717109b5e892d72f8e"), tasks.bytes(196608)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/org/lwjgl/lwjgl-jemalloc/3.3.3/lwjgl-jemalloc-3.3.3.jar"),
    tasks.sha1("b543467b7ff3c6920539a88ee602d34098628be5"), tasks.bytes(65536)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/org/lwjgl/lwjgl-openal/3.3.3/lwjgl-openal-3.3.3.jar"),
    tasks.sha1("daada81ceb5fc0c291fbfdd4433cb8d9423577f2"), tasks.bytes(131072)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/org/lwjgl/lwjgl-opengl/3.3.3/lwjgl-opengl-3.3.3.jar"),
    tasks.sha1("02f6b0147078396a58979125a4c947664e98293a"), tasks.bytes(983040)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/org/lwjgl/lwjgl-stb/3.3.3/lwjgl-stb-3.3.3.jar"),
    tasks.sha1("25dd6161988d7e65f71d5065c99902402ee32746"), tasks.bytes(131072)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/org/lwjgl/lwjgl-tinyfd/3.3.3/lwjgl-tinyfd-3.3.3.jar"),
    tasks.sha1("82d755ca94b102e9ca77283b9e2dc46d1b15fbe5"), tasks.bytes(65536)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3.jar"),
    tasks.sha1("29589b5f87ed335a6c7e7ee6a5775f81f97ecb84"), tasks.bytes(786432)));
tasks.add_classpath(tasks.fetch_jar(
    tasks.https_url("https://libraries.minecraft.net/org/slf4j/slf4j-api/2.0.17/slf4j-api-2.0.17.jar"),
    tasks.sha1("d9e58ac9c7779ba3bf8142aff6c830617a7fe60f"), tasks.bytes(131072)));

// --- natives ---------------------------------------------------------------------------------
// One `.so` per jar, each under its own prefix, stripped flat so `-Djava.library.path` finds
// them all in one directory. Merged rather than added one at a time because a runtime directory
// is one tree and the terminal takes one.

let natives = tasks.extract_files(tasks.fetch_jar(
        tasks.https_url("https://libraries.minecraft.net/com/mojang/jtracy/1.0.37/jtracy-1.0.37-natives-linux.jar"),
        tasks.sha1("e1b4395227af41195da9e2ead13c48ee0b7d31cd"), tasks.bytes(196608)), "");
natives = tasks.merge_trees(natives,
    tasks.extract_files(tasks.fetch_jar(
        tasks.https_url("https://libraries.minecraft.net/org/lwjgl/lwjgl-freetype/3.3.3/lwjgl-freetype-3.3.3-natives-linux.jar"),
        tasks.sha1("149070a5480900347071b7074779531f25a6e3dc"), tasks.bytes(1245184)), "linux/x64/org/lwjgl/freetype"));
natives = tasks.merge_trees(natives,
    tasks.extract_files(tasks.fetch_jar(
        tasks.https_url("https://libraries.minecraft.net/org/lwjgl/lwjgl-glfw/3.3.3/lwjgl-glfw-3.3.3-natives-linux.jar"),
        tasks.sha1("a03684c5e4b1b1dbbe0d29dbbdc27b985b6840f2"), tasks.bytes(131072)), "linux/x64/org/lwjgl/glfw"));
natives = tasks.merge_trees(natives,
    tasks.extract_files(tasks.fetch_jar(
        tasks.https_url("https://libraries.minecraft.net/org/lwjgl/lwjgl-jemalloc/3.3.3/lwjgl-jemalloc-3.3.3-natives-linux.jar"),
        tasks.sha1("4f86728bf449b1dd61251c4e0ac01df1389cb51e"), tasks.bytes(262144)), "linux/x64/org/lwjgl/jemalloc"));
natives = tasks.merge_trees(natives,
    tasks.extract_files(tasks.fetch_jar(
        tasks.https_url("https://libraries.minecraft.net/org/lwjgl/lwjgl-openal/3.3.3/lwjgl-openal-3.3.3-natives-linux.jar"),
        tasks.sha1("3037360cc4595079bea240af250b6d1a527e0905"), tasks.bytes(589824)), "linux/x64/org/lwjgl/openal"));
natives = tasks.merge_trees(natives,
    tasks.extract_files(tasks.fetch_jar(
        tasks.https_url("https://libraries.minecraft.net/org/lwjgl/lwjgl-opengl/3.3.3/lwjgl-opengl-3.3.3-natives-linux.jar"),
        tasks.sha1("62c70a4b00ca5391882b0f4b787c1588d24f1c86"), tasks.bytes(131072)), "linux/x64/org/lwjgl/opengl"));
natives = tasks.merge_trees(natives,
    tasks.extract_files(tasks.fetch_jar(
        tasks.https_url("https://libraries.minecraft.net/org/lwjgl/lwjgl-stb/3.3.3/lwjgl-stb-3.3.3-natives-linux.jar"),
        tasks.sha1("fd1271ccd9d85eff2fa31f3fd543e02ccfaf5041"), tasks.bytes(262144)), "linux/x64/org/lwjgl/stb"));
natives = tasks.merge_trees(natives,
    tasks.extract_files(tasks.fetch_jar(
        tasks.https_url("https://libraries.minecraft.net/org/lwjgl/lwjgl-tinyfd/3.3.3/lwjgl-tinyfd-3.3.3-natives-linux.jar"),
        tasks.sha1("d8d58daa0c3e5fd906fee96f5fddbcbc07cc308b"), tasks.bytes(65536)), "linux/x64/org/lwjgl/tinyfd"));
natives = tasks.merge_trees(natives,
    tasks.extract_files(tasks.fetch_jar(
        tasks.https_url("https://libraries.minecraft.net/org/lwjgl/lwjgl/3.3.3/lwjgl-3.3.3-natives-linux.jar"),
        tasks.sha1("1713758e3660ba66e1e954396fd18126038b33c0"), tasks.bytes(131072)), "linux/x64/org/lwjgl"));
tasks.add_runtime_dir("natives", natives);

// --- assets ----------------------------------------------------------------------------------
// 45 objects, 15.8 MiB. The full store is 4591 objects and 430 MiB; the difference is the
// sounds and the other 142 languages, neither of which a screenshot shows.

let assets = tasks.place("indexes/29.json", tasks.fetch_bytes(
    tasks.https_url("https://piston-meta.mojang.com/v1/packages/e9eeaade32639d73d4f7a30a007ace472c61cbd6/29.json"),
    tasks.sha1("e9eeaade32639d73d4f7a30a007ace472c61cbd6"), tasks.bytes(589824)));
assets = tasks.merge_trees(assets, tasks.place("objects/b6/b62ca8ec10d07e6bf5ac8dae0c8c1d2e6a1e3356", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/b6/b62ca8ec10d07e6bf5ac8dae0c8c1d2e6a1e3356"),
    tasks.sha1("b62ca8ec10d07e6bf5ac8dae0c8c1d2e6a1e3356"), tasks.bytes(65536))));
assets = tasks.merge_trees(assets, tasks.place("objects/5f/5ff04807c356f1beed0b86ccf659b44b9983e3fa", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/5f/5ff04807c356f1beed0b86ccf659b44b9983e3fa"),
    tasks.sha1("5ff04807c356f1beed0b86ccf659b44b9983e3fa"), tasks.bytes(65536))));
assets = tasks.merge_trees(assets, tasks.place("objects/80/8030dd9dc315c0381d52c4782ea36c6baf6e8135", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/80/8030dd9dc315c0381d52c4782ea36c6baf6e8135"),
    tasks.sha1("8030dd9dc315c0381d52c4782ea36c6baf6e8135"), tasks.bytes(65536))));
assets = tasks.merge_trees(assets, tasks.place("objects/af/af96f55a90eaf11b327f1b5f8834a051027dc506", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/af/af96f55a90eaf11b327f1b5f8834a051027dc506"),
    tasks.sha1("af96f55a90eaf11b327f1b5f8834a051027dc506"), tasks.bytes(65536))));
assets = tasks.merge_trees(assets, tasks.place("objects/b8/b80b6e9ff01c78c624df5429e1d3dcd3d5130834", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/b8/b80b6e9ff01c78c624df5429e1d3dcd3d5130834"),
    tasks.sha1("b80b6e9ff01c78c624df5429e1d3dcd3d5130834"), tasks.bytes(65536))));
assets = tasks.merge_trees(assets, tasks.place("objects/f0/f00657542252858a721e715a2e888a9226404e35", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/f0/f00657542252858a721e715a2e888a9226404e35"),
    tasks.sha1("f00657542252858a721e715a2e888a9226404e35"), tasks.bytes(393216))));
assets = tasks.merge_trees(assets, tasks.place("objects/95/958d57021d8009de55d6e9e19957a72545e3c30c", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/95/958d57021d8009de55d6e9e19957a72545e3c30c"),
    tasks.sha1("958d57021d8009de55d6e9e19957a72545e3c30c"), tasks.bytes(65536))));
assets = tasks.merge_trees(assets, tasks.place("objects/94/949afe72d4d3d785dab52d8baaefeb0e38b3c067", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/94/949afe72d4d3d785dab52d8baaefeb0e38b3c067"),
    tasks.sha1("949afe72d4d3d785dab52d8baaefeb0e38b3c067"), tasks.bytes(65536))));
assets = tasks.merge_trees(assets, tasks.place("objects/9f/9f84f917a09facacf1235eed3fa77789e4554afb", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/9f/9f84f917a09facacf1235eed3fa77789e4554afb"),
    tasks.sha1("9f84f917a09facacf1235eed3fa77789e4554afb"), tasks.bytes(65536))));
assets = tasks.merge_trees(assets, tasks.place("objects/26/26ad18d9f4ef0a71255459b5f01b738b81dbc7dc", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/26/26ad18d9f4ef0a71255459b5f01b738b81dbc7dc"),
    tasks.sha1("26ad18d9f4ef0a71255459b5f01b738b81dbc7dc"), tasks.bytes(65536))));
assets = tasks.merge_trees(assets, tasks.place("objects/df/df274fe57c49ef1af6d218703d805db76a5c8af9", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/df/df274fe57c49ef1af6d218703d805db76a5c8af9"),
    tasks.sha1("df274fe57c49ef1af6d218703d805db76a5c8af9"), tasks.bytes(65536))));
assets = tasks.merge_trees(assets, tasks.place("objects/65/65ebca3306ccd6d7f9d5de8f1cc7a1216d80246d", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/65/65ebca3306ccd6d7f9d5de8f1cc7a1216d80246d"),
    tasks.sha1("65ebca3306ccd6d7f9d5de8f1cc7a1216d80246d"), tasks.bytes(327680))));
assets = tasks.merge_trees(assets, tasks.place("objects/40/402ded0eebd448033ef415e861a17513075f80e7", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/40/402ded0eebd448033ef415e861a17513075f80e7"),
    tasks.sha1("402ded0eebd448033ef415e861a17513075f80e7"), tasks.bytes(65536))));
assets = tasks.merge_trees(assets, tasks.place("objects/db/db5582c177aca966ba1a9b062cead233a262a939", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/db/db5582c177aca966ba1a9b062cead233a262a939"),
    tasks.sha1("db5582c177aca966ba1a9b062cead233a262a939"), tasks.bytes(65536))));
assets = tasks.merge_trees(assets, tasks.place("objects/cc/ccd5ac4767ce0a9c71d1dd62f2dc25449789b5dd", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/cc/ccd5ac4767ce0a9c71d1dd62f2dc25449789b5dd"),
    tasks.sha1("ccd5ac4767ce0a9c71d1dd62f2dc25449789b5dd"), tasks.bytes(1572864))));
assets = tasks.merge_trees(assets, tasks.place("objects/59/590470ab0f17afb73a4e41d9cb56fdbe069d275a", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/59/590470ab0f17afb73a4e41d9cb56fdbe069d275a"),
    tasks.sha1("590470ab0f17afb73a4e41d9cb56fdbe069d275a"), tasks.bytes(262144))));
assets = tasks.merge_trees(assets, tasks.place("objects/d7/d7caa0e3aa5eb656c51817ae4bfbf3c4d72cdaad", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/d7/d7caa0e3aa5eb656c51817ae4bfbf3c4d72cdaad"),
    tasks.sha1("d7caa0e3aa5eb656c51817ae4bfbf3c4d72cdaad"), tasks.bytes(131072))));
assets = tasks.merge_trees(assets, tasks.place("objects/5b/5b822601569264742c5a1a00baf808acc4e83a0c", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/5b/5b822601569264742c5a1a00baf808acc4e83a0c"),
    tasks.sha1("5b822601569264742c5a1a00baf808acc4e83a0c"), tasks.bytes(65536))));
assets = tasks.merge_trees(assets, tasks.place("objects/24/24e1b0a4692ff3379226ae300009cf124b992b79", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/24/24e1b0a4692ff3379226ae300009cf124b992b79"),
    tasks.sha1("24e1b0a4692ff3379226ae300009cf124b992b79"), tasks.bytes(720896))));
assets = tasks.merge_trees(assets, tasks.place("objects/12/123370fbb15bf1e85cf5038b3e8ca719ad391dfb", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/12/123370fbb15bf1e85cf5038b3e8ca719ad391dfb"),
    tasks.sha1("123370fbb15bf1e85cf5038b3e8ca719ad391dfb"), tasks.bytes(589824))));
assets = tasks.merge_trees(assets, tasks.place("objects/af/af28e43df0b60875348cecd1c1f65e2325b54204", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/af/af28e43df0b60875348cecd1c1f65e2325b54204"),
    tasks.sha1("af28e43df0b60875348cecd1c1f65e2325b54204"), tasks.bytes(655360))));
assets = tasks.merge_trees(assets, tasks.place("objects/15/15e3d77a4555455d752fde55b2661c159bee59ba", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/15/15e3d77a4555455d752fde55b2661c159bee59ba"),
    tasks.sha1("15e3d77a4555455d752fde55b2661c159bee59ba"), tasks.bytes(720896))));
assets = tasks.merge_trees(assets, tasks.place("objects/bc/bc648a153191d8490fbaaa49e89c62240c6b2936", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/bc/bc648a153191d8490fbaaa49e89c62240c6b2936"),
    tasks.sha1("bc648a153191d8490fbaaa49e89c62240c6b2936"), tasks.bytes(524288))));
assets = tasks.merge_trees(assets, tasks.place("objects/79/79795659716b1f0ad2b2b3078933acb7c86327d9", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/79/79795659716b1f0ad2b2b3078933acb7c86327d9"),
    tasks.sha1("79795659716b1f0ad2b2b3078933acb7c86327d9"), tasks.bytes(524288))));
assets = tasks.merge_trees(assets, tasks.place("objects/64/64d2b763bb3bcfcaee67ba1b9351b0a83c209ae7", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/64/64d2b763bb3bcfcaee67ba1b9351b0a83c209ae7"),
    tasks.sha1("64d2b763bb3bcfcaee67ba1b9351b0a83c209ae7"), tasks.bytes(262144))));
assets = tasks.merge_trees(assets, tasks.place("objects/4e/4ef46fd3165dcd588f4b2d945d4aacca9ad90d42", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/4e/4ef46fd3165dcd588f4b2d945d4aacca9ad90d42"),
    tasks.sha1("4ef46fd3165dcd588f4b2d945d4aacca9ad90d42"), tasks.bytes(262144))));
assets = tasks.merge_trees(assets, tasks.place("objects/9d/9dd32387135eefa7ab95996d52a5ca4cec8a3b30", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/9d/9dd32387135eefa7ab95996d52a5ca4cec8a3b30"),
    tasks.sha1("9dd32387135eefa7ab95996d52a5ca4cec8a3b30"), tasks.bytes(65536))));
assets = tasks.merge_trees(assets, tasks.place("objects/6c/6cb57c8bf7f43ecb75339be01e23b5be0c83f0c9", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/6c/6cb57c8bf7f43ecb75339be01e23b5be0c83f0c9"),
    tasks.sha1("6cb57c8bf7f43ecb75339be01e23b5be0c83f0c9"), tasks.bytes(65536))));
assets = tasks.merge_trees(assets, tasks.place("objects/a1/a1260ad0107b49e9ea070a1ed87934b50d6caefd", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/a1/a1260ad0107b49e9ea070a1ed87934b50d6caefd"),
    tasks.sha1("a1260ad0107b49e9ea070a1ed87934b50d6caefd"), tasks.bytes(65536))));
assets = tasks.merge_trees(assets, tasks.place("objects/45/4574e4b3a592117b019e05088947ee26bc55e697", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/45/4574e4b3a592117b019e05088947ee26bc55e697"),
    tasks.sha1("4574e4b3a592117b019e05088947ee26bc55e697"), tasks.bytes(655360))));
assets = tasks.merge_trees(assets, tasks.place("objects/fd/fdf18a3512fd1d2ce62b19865d4312fcc343cd36", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/fd/fdf18a3512fd1d2ce62b19865d4312fcc343cd36"),
    tasks.sha1("fdf18a3512fd1d2ce62b19865d4312fcc343cd36"), tasks.bytes(786432))));
assets = tasks.merge_trees(assets, tasks.place("objects/1d/1d8eb3f824e8e17c0834a17cf39b9e55b8479c2e", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/1d/1d8eb3f824e8e17c0834a17cf39b9e55b8479c2e"),
    tasks.sha1("1d8eb3f824e8e17c0834a17cf39b9e55b8479c2e"), tasks.bytes(524288))));
assets = tasks.merge_trees(assets, tasks.place("objects/2d/2d888ac14e51d7cab64b5e735456d25bf000720e", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/2d/2d888ac14e51d7cab64b5e735456d25bf000720e"),
    tasks.sha1("2d888ac14e51d7cab64b5e735456d25bf000720e"), tasks.bytes(655360))));
assets = tasks.merge_trees(assets, tasks.place("objects/de/deba0627f49ca561aaec8b61057dac00deb83fe2", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/de/deba0627f49ca561aaec8b61057dac00deb83fe2"),
    tasks.sha1("deba0627f49ca561aaec8b61057dac00deb83fe2"), tasks.bytes(655360))));
assets = tasks.merge_trees(assets, tasks.place("objects/4a/4a101c336082c530e93945b0e31ac818cfbf78a0", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/4a/4a101c336082c530e93945b0e31ac818cfbf78a0"),
    tasks.sha1("4a101c336082c530e93945b0e31ac818cfbf78a0"), tasks.bytes(786432))));
assets = tasks.merge_trees(assets, tasks.place("objects/52/529e4602466f3752d66bf34eb8e5b92f7d602ccc", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/52/529e4602466f3752d66bf34eb8e5b92f7d602ccc"),
    tasks.sha1("529e4602466f3752d66bf34eb8e5b92f7d602ccc"), tasks.bytes(589824))));
assets = tasks.merge_trees(assets, tasks.place("objects/15/15c8e87c9ace8c279dffaf86d05dda43dd51d933", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/15/15c8e87c9ace8c279dffaf86d05dda43dd51d933"),
    tasks.sha1("15c8e87c9ace8c279dffaf86d05dda43dd51d933"), tasks.bytes(720896))));
assets = tasks.merge_trees(assets, tasks.place("objects/01/01564e4a93458fdc78544743528f31bd0ee1335e", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/01/01564e4a93458fdc78544743528f31bd0ee1335e"),
    tasks.sha1("01564e4a93458fdc78544743528f31bd0ee1335e"), tasks.bytes(720896))));
assets = tasks.merge_trees(assets, tasks.place("objects/44/44f46567ae7329a71a2e9bfa8702662ba9e8be38", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/44/44f46567ae7329a71a2e9bfa8702662ba9e8be38"),
    tasks.sha1("44f46567ae7329a71a2e9bfa8702662ba9e8be38"), tasks.bytes(720896))));
assets = tasks.merge_trees(assets, tasks.place("objects/ee/eed3b961158ebeae781e6c737ee55a1f5e8f6c9d", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/ee/eed3b961158ebeae781e6c737ee55a1f5e8f6c9d"),
    tasks.sha1("eed3b961158ebeae781e6c737ee55a1f5e8f6c9d"), tasks.bytes(262144))));
assets = tasks.merge_trees(assets, tasks.place("objects/c0/c05cbd8c88b5878703898150e157af88a29d8091", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/c0/c05cbd8c88b5878703898150e157af88a29d8091"),
    tasks.sha1("c05cbd8c88b5878703898150e157af88a29d8091"), tasks.bytes(786432))));
assets = tasks.merge_trees(assets, tasks.place("objects/09/09fc4bb8b59416d1bd941aa1f3cda0e58d8b1e38", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/09/09fc4bb8b59416d1bd941aa1f3cda0e58d8b1e38"),
    tasks.sha1("09fc4bb8b59416d1bd941aa1f3cda0e58d8b1e38"), tasks.bytes(524288))));
assets = tasks.merge_trees(assets, tasks.place("objects/a4/a449a2395b194c67b17ce2cba15b1e2958130f11", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/a4/a449a2395b194c67b17ce2cba15b1e2958130f11"),
    tasks.sha1("a449a2395b194c67b17ce2cba15b1e2958130f11"), tasks.bytes(524288))));
assets = tasks.merge_trees(assets, tasks.place("objects/c1/c106e5ef000809049ce7d56e858898255cff9223", tasks.fetch_bytes(
    tasks.https_url("https://resources.download.minecraft.net/c1/c106e5ef000809049ce7d56e858898255cff9223"),
    tasks.sha1("c106e5ef000809049ce7d56e858898255cff9223"), tasks.bytes(917504))));
tasks.add_runtime_dir("assets", assets);
