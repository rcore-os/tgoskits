use num_enum::{FromPrimitive, IntoPrimitive};

#[repr(u16)]
#[derive(Debug, PartialEq, Eq, IntoPrimitive, FromPrimitive, Clone, Copy)]
pub enum LanguageId {
    /// Afrikaans
    Afrikaans = 0x0436,
    /// Albanian
    Albanian = 0x041c,
    /// Arabic (Saudi Arabia)
    ArabicSaudiArabia = 0x0401,
    /// Arabic (Iraq)
    ArabicIraq = 0x0801,
    /// Arabic (Egypt)
    ArabicEgypt = 0x0c01,
    /// Arabic (Libya)
    ArabicLibya = 0x1001,
    /// Arabic (Algeria)
    ArabicAlgeria = 0x1401,
    /// Arabic (Morocco)
    ArabicMorocco = 0x1801,
    /// Arabic (Tunisia)
    ArabicTunisia = 0x1c01,
    /// Arabic (Oman)
    ArabicOman = 0x2001,
    /// Arabic (Yemen)
    ArabicYemen = 0x2401,
    /// Arabic (Syria)
    ArabicSyria = 0x2801,
    /// Arabic (Jordan)
    ArabicJordan = 0x2c01,
    /// Arabic (Lebanon)
    ArabicLebanon = 0x3001,
    /// Arabic (Kuwait)
    ArabicKuwait = 0x3401,
    /// Arabic (U.A.E.)
    ArabicUAE = 0x3801,
    /// Arabic (Bahrain)
    ArabicBahrain = 0x3c01,
    /// Arabic (Qatar)
    ArabicQatar = 0x4001,
    /// Armenian
    Armenian = 0x042b,
    /// Assamese
    Assamese = 0x044d,
    /// Azeri (Latin)
    AzeriLatin = 0x042c,
    /// Azeri (Cyrillic)
    AzeriCyrillic = 0x082c,
    /// Basque
    Basque = 0x042d,
    /// Belarussian
    Belarussian = 0x0423,
    /// Bengali
    Bengali = 0x0445,
    /// Bulgarian
    Bulgarian = 0x0402,
    /// Burmese
    Burmese = 0x0455,
    /// Catalan
    Catalan = 0x0403,
    /// Chinese (Taiwan)
    ChineseTaiwan = 0x0404,
    /// Chinese (PRC)
    ChinesePRC = 0x0804,
    /// Chinese (Hong Kong SAR, PRC)
    ChineseHongKong = 0x0c04,
    /// Chinese (Singapore)
    ChineseSingapore = 0x1004,
    /// Chinese (Macau SAR)
    ChineseMacau = 0x1404,
    /// Croatian
    Croatian = 0x041a,
    /// Czech
    Czech = 0x0405,
    /// Danish
    Danish = 0x0406,
    /// Dutch (Netherlands)
    DutchNetherlands = 0x0413,
    /// Dutch (Belgium)
    DutchBelgium = 0x0813,
    /// English (United States)
    EnglishUnitedStates = 0x0409,
    /// English (United Kingdom)
    EnglishUnitedKingdom = 0x0809,
    /// English (Australian)
    EnglishAustralian = 0x0c09,
    /// English (Canadian)
    EnglishCanadian = 0x1009,
    /// English (New Zealand)
    EnglishNewZealand = 0x1409,
    /// English (Ireland)
    EnglishIreland = 0x1809,
    /// English (South Africa)
    EnglishSouthAfrica = 0x1c09,
    /// English (Jamaica)
    EnglishJamaica = 0x2009,
    /// English (Caribbean)
    EnglishCaribbean = 0x2409,
    /// English (Belize)
    EnglishBelize = 0x2809,
    /// English (Trinidad)
    EnglishTrinidad = 0x2c09,
    /// English (Zimbabwe)
    EnglishZimbabwe = 0x3009,
    /// English (Philippines)
    EnglishPhilippines = 0x3409,
    /// Estonian
    Estonian = 0x0425,
    /// Faeroese
    Faeroese = 0x0438,
    /// Farsi
    Farsi = 0x0429,
    /// Finnish
    Finnish = 0x040b,
    /// French (Standard)
    FrenchStandard = 0x040c,
    /// French (Belgian)
    FrenchBelgian = 0x080c,
    /// French (Canadian)
    FrenchCanadian = 0x0c0c,
    /// French (Switzerland)
    FrenchSwitzerland = 0x100c,
    /// French (Luxembourg)
    FrenchLuxembourg = 0x140c,
    /// French (Monaco)
    FrenchMonaco = 0x180c,
    /// Georgian
    Georgian = 0x0437,
    /// German (Standard)
    GermanStandard = 0x0407,
    /// German (Switzerland)
    GermanSwitzerland = 0x0807,
    /// German (Austria)
    GermanAustria = 0x0c07,
    /// German (Luxembourg)
    GermanLuxembourg = 0x1007,
    /// German (Liechtenstein)
    GermanLiechtenstein = 0x1407,
    /// Greek
    Greek = 0x0408,
    /// Gujarati
    Gujarati = 0x0447,
    /// Hebrew
    Hebrew = 0x040d,
    /// Hindi
    Hindi = 0x0439,
    /// Hungarian
    Hungarian = 0x040e,
    /// Icelandic
    Icelandic = 0x040f,
    /// Indonesian
    Indonesian = 0x0421,
    /// Italian (Standard)
    ItalianStandard = 0x0410,
    /// Italian (Switzerland)
    ItalianSwitzerland = 0x0810,
    /// Japanese
    Japanese = 0x0411,
    /// Kannada
    Kannada = 0x044b,
    /// Kashmiri (India)
    KashmiriIndia = 0x0860,
    /// Kazakh
    Kazakh = 0x043f,
    /// Konkani
    Konkani = 0x0457,
    /// Korean
    Korean = 0x0412,
    /// Korean (Johab)
    KoreanJohab = 0x0812,
    /// Latvian
    Latvian = 0x0426,
    /// Lithuanian
    Lithuanian = 0x0427,
    /// Lithuanian (Classic)
    LithuanianClassic = 0x0827,
    /// Macedonian
    Macedonian = 0x042f,
    /// Malay (Malaysian)
    MalayMalaysian = 0x043e,
    /// Malay (Brunei Darussalam)
    MalayBrunei = 0x083e,
    /// Malayalam
    Malayalam = 0x044c,
    /// Manipuri
    Manipuri = 0x0458,
    /// Marathi
    Marathi = 0x044e,
    /// Nepali (India)
    NepaliIndia = 0x0861,
    /// Norwegian (Bokmal)
    NorwegianBokmal = 0x0414,
    /// Norwegian (Nynorsk)
    NorwegianNynorsk = 0x0814,
    /// Oriya
    Oriya = 0x0448,
    /// Polish
    Polish = 0x0415,
    /// Portuguese (Brazil)
    PortugueseBrazil = 0x0416,
    /// Portuguese (Standard)
    PortugueseStandard = 0x0816,
    /// Punjabi
    Punjabi = 0x0446,
    /// Romanian
    Romanian = 0x0418,
    /// Russian
    Russian = 0x0419,
    /// Sanskrit
    Sanskrit = 0x044f,
    /// Serbian (Cyrillic)
    SerbianCyrillic = 0x0c1a,
    /// Serbian (Latin)
    SerbianLatin = 0x081a,
    /// Sindhi
    Sindhi = 0x0459,
    /// Slovak
    Slovak = 0x041b,
    /// Slovenian
    Slovenian = 0x0424,
    /// Spanish (Traditional Sort)
    SpanishTraditionalSort = 0x040a,
    /// Spanish (Mexican)
    SpanishMexican = 0x080a,
    /// Spanish (Modern Sort)
    SpanishModernSort = 0x0c0a,
    /// Spanish (Guatemala)
    SpanishGuatemala = 0x100a,
    /// Spanish (Costa Rica)
    SpanishCostaRica = 0x140a,
    /// Spanish (Panama)
    SpanishPanama = 0x180a,
    /// Spanish (Dominican Republic)
    SpanishDominicanRepublic = 0x1c0a,
    /// Spanish (Venezuela)
    SpanishVenezuela = 0x200a,
    /// Spanish (Colombia)
    SpanishColombia = 0x240a,
    /// Spanish (Peru)
    SpanishPeru = 0x280a,
    /// Spanish (Argentina)
    SpanishArgentina = 0x2c0a,
    /// Spanish (Ecuador)
    SpanishEcuador = 0x300a,
    /// Spanish (Chile)
    SpanishChile = 0x340a,
    /// Spanish (Uruguay)
    SpanishUruguay = 0x380a,
    /// Spanish (Paraguay)
    SpanishParaguay = 0x3c0a,
    /// Spanish (Bolivia)
    SpanishBolivia = 0x400a,
    /// Spanish (El Salvador)
    SpanishElSalvador = 0x440a,
    /// Spanish (Honduras)
    SpanishHonduras = 0x480a,
    /// Spanish (Nicaragua)
    SpanishNicaragua = 0x4c0a,
    /// Spanish (Puerto Rico)
    SpanishPuertoRico = 0x500a,
    /// Sutu
    Sutu = 0x0430,
    /// Swahili (Kenya)
    SwahiliKenya = 0x0441,
    /// Swedish
    Swedish = 0x041d,
    /// Swedish (Finland)
    SwedishFinland = 0x081d,
    /// Tamil
    Tamil = 0x0449,
    /// Tatar (Tatarstan)
    TatarTatarstan = 0x0444,
    /// Telugu
    Telugu = 0x044a,
    /// Thai
    Thai = 0x041e,
    /// Turkish
    Turkish = 0x041f,
    /// Ukrainian
    Ukrainian = 0x0422,
    /// Urdu (Pakistan)
    UrduPakistan = 0x0420,
    /// Urdu (India)
    UrduIndia = 0x0820,
    /// Uzbek (Latin)
    UzbekLatin = 0x0443,
    /// Uzbek (Cyrillic)
    UzbekCyrillic = 0x0843,
    /// Vietnamese
    Vietnamese = 0x042a,
    /// HID (Usage Data Descriptor)
    HidUsageDataDescriptor = 0x04ff,
    /// HID (Vendor Defined 1)
    HidVendorDefined1 = 0xf0ff,
    /// HID (Vendor Defined 2)
    HidVendorDefined2 = 0xf4ff,
    /// HID (Vendor Defined 3)
    HidVendorDefined3 = 0xf8ff,
    /// HID (Vendor Defined 4)
    HidVendorDefined4 = 0xfcff,
    #[num_enum(catch_all)]
    Other(u16),
}

#[allow(clippy::derivable_impls)]
impl Default for LanguageId {
    fn default() -> Self {
        LanguageId::EnglishUnitedStates
    }
}
