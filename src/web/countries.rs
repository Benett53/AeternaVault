//! Countries by ISO 3166-1 alpha-2 code, with English and German names, for
//! choosing countries in geoblocking.

const TABLE: &str = "\
AD|Andorra|Andorra
AE|United Arab Emirates|Vereinigte Arabische Emirate
AF|Afghanistan|Afghanistan
AG|Antigua and Barbuda|Antigua und Barbuda
AI|Anguilla|Anguilla
AL|Albania|Albanien
AM|Armenia|Armenien
AO|Angola|Angola
AQ|Antarctica|Antarktis
AR|Argentina|Argentinien
AS|American Samoa|Amerikanisch-Samoa
AT|Austria|Österreich
AU|Australia|Australien
AW|Aruba|Aruba
AX|Åland Islands|Ålandinseln
AZ|Azerbaijan|Aserbaidschan
BA|Bosnia and Herzegovina|Bosnien und Herzegowina
BB|Barbados|Barbados
BD|Bangladesh|Bangladesch
BE|Belgium|Belgien
BF|Burkina Faso|Burkina Faso
BG|Bulgaria|Bulgarien
BH|Bahrain|Bahrain
BI|Burundi|Burundi
BJ|Benin|Benin
BL|Saint Barthélemy|Saint-Barthélemy
BM|Bermuda|Bermuda
BN|Brunei|Brunei
BO|Bolivia|Bolivien
BQ|Caribbean Netherlands|Karibische Niederlande
BR|Brazil|Brasilien
BS|Bahamas|Bahamas
BT|Bhutan|Bhutan
BV|Bouvet Island|Bouvetinsel
BW|Botswana|Botsuana
BY|Belarus|Belarus
BZ|Belize|Belize
CA|Canada|Kanada
CC|Cocos (Keeling) Islands|Kokosinseln
CD|Congo (Democratic Republic)|Kongo (Demokratische Republik)
CF|Central African Republic|Zentralafrikanische Republik
CG|Congo|Kongo
CH|Switzerland|Schweiz
CI|Côte d'Ivoire|Elfenbeinküste
CK|Cook Islands|Cookinseln
CL|Chile|Chile
CM|Cameroon|Kamerun
CN|China|China
CO|Colombia|Kolumbien
CR|Costa Rica|Costa Rica
CU|Cuba|Kuba
CV|Cape Verde|Kap Verde
CW|Curaçao|Curaçao
CX|Christmas Island|Weihnachtsinsel
CY|Cyprus|Zypern
CZ|Czechia|Tschechien
DE|Germany|Deutschland
DJ|Djibouti|Dschibuti
DK|Denmark|Dänemark
DM|Dominica|Dominica
DO|Dominican Republic|Dominikanische Republik
DZ|Algeria|Algerien
EC|Ecuador|Ecuador
EE|Estonia|Estland
EG|Egypt|Ägypten
EH|Western Sahara|Westsahara
ER|Eritrea|Eritrea
ES|Spain|Spanien
ET|Ethiopia|Äthiopien
FI|Finland|Finnland
FJ|Fiji|Fidschi
FK|Falkland Islands|Falklandinseln
FM|Micronesia|Mikronesien
FO|Faroe Islands|Färöer
FR|France|Frankreich
GA|Gabon|Gabun
GB|United Kingdom|Vereinigtes Königreich
GD|Grenada|Grenada
GE|Georgia|Georgien
GF|French Guiana|Französisch-Guayana
GG|Guernsey|Guernsey
GH|Ghana|Ghana
GI|Gibraltar|Gibraltar
GL|Greenland|Grönland
GM|Gambia|Gambia
GN|Guinea|Guinea
GP|Guadeloupe|Guadeloupe
GQ|Equatorial Guinea|Äquatorialguinea
GR|Greece|Griechenland
GS|South Georgia and the South Sandwich Islands|Südgeorgien und die Südlichen Sandwichinseln
GT|Guatemala|Guatemala
GU|Guam|Guam
GW|Guinea-Bissau|Guinea-Bissau
GY|Guyana|Guyana
HK|Hong Kong|Hongkong
HM|Heard Island and McDonald Islands|Heard und McDonaldinseln
HN|Honduras|Honduras
HR|Croatia|Kroatien
HT|Haiti|Haiti
HU|Hungary|Ungarn
ID|Indonesia|Indonesien
IE|Ireland|Irland
IL|Israel|Israel
IM|Isle of Man|Isle of Man
IN|India|Indien
IO|British Indian Ocean Territory|Britisches Territorium im Indischen Ozean
IQ|Iraq|Irak
IR|Iran|Iran
IS|Iceland|Island
IT|Italy|Italien
JE|Jersey|Jersey
JM|Jamaica|Jamaika
JO|Jordan|Jordanien
JP|Japan|Japan
KE|Kenya|Kenia
KG|Kyrgyzstan|Kirgisistan
KH|Cambodia|Kambodscha
KI|Kiribati|Kiribati
KM|Comoros|Komoren
KN|Saint Kitts and Nevis|St. Kitts und Nevis
KP|North Korea|Nordkorea
KR|South Korea|Südkorea
KW|Kuwait|Kuwait
KY|Cayman Islands|Kaimaninseln
KZ|Kazakhstan|Kasachstan
LA|Laos|Laos
LB|Lebanon|Libanon
LC|Saint Lucia|St. Lucia
LI|Liechtenstein|Liechtenstein
LK|Sri Lanka|Sri Lanka
LR|Liberia|Liberia
LS|Lesotho|Lesotho
LT|Lithuania|Litauen
LU|Luxembourg|Luxemburg
LV|Latvia|Lettland
LY|Libya|Libyen
MA|Morocco|Marokko
MC|Monaco|Monaco
MD|Moldova|Moldau
ME|Montenegro|Montenegro
MF|Saint Martin|Saint-Martin
MG|Madagascar|Madagaskar
MH|Marshall Islands|Marshallinseln
MK|North Macedonia|Nordmazedonien
ML|Mali|Mali
MM|Myanmar|Myanmar
MN|Mongolia|Mongolei
MO|Macao|Macau
MP|Northern Mariana Islands|Nördliche Marianen
MQ|Martinique|Martinique
MR|Mauritania|Mauretanien
MS|Montserrat|Montserrat
MT|Malta|Malta
MU|Mauritius|Mauritius
MV|Maldives|Malediven
MW|Malawi|Malawi
MX|Mexico|Mexiko
MY|Malaysia|Malaysia
MZ|Mozambique|Mosambik
NA|Namibia|Namibia
NC|New Caledonia|Neukaledonien
NE|Niger|Niger
NF|Norfolk Island|Norfolkinsel
NG|Nigeria|Nigeria
NI|Nicaragua|Nicaragua
NL|Netherlands|Niederlande
NO|Norway|Norwegen
NP|Nepal|Nepal
NR|Nauru|Nauru
NU|Niue|Niue
NZ|New Zealand|Neuseeland
OM|Oman|Oman
PA|Panama|Panama
PE|Peru|Peru
PF|French Polynesia|Französisch-Polynesien
PG|Papua New Guinea|Papua-Neuguinea
PH|Philippines|Philippinen
PK|Pakistan|Pakistan
PL|Poland|Polen
PM|Saint Pierre and Miquelon|Saint-Pierre und Miquelon
PN|Pitcairn Islands|Pitcairninseln
PR|Puerto Rico|Puerto Rico
PS|Palestine|Palästina
PT|Portugal|Portugal
PW|Palau|Palau
PY|Paraguay|Paraguay
QA|Qatar|Katar
RE|Réunion|Réunion
RO|Romania|Rumänien
RS|Serbia|Serbien
RU|Russia|Russland
RW|Rwanda|Ruanda
SA|Saudi Arabia|Saudi-Arabien
SB|Solomon Islands|Salomonen
SC|Seychelles|Seychellen
SD|Sudan|Sudan
SE|Sweden|Schweden
SG|Singapore|Singapur
SH|Saint Helena|St. Helena
SI|Slovenia|Slowenien
SJ|Svalbard and Jan Mayen|Svalbard und Jan Mayen
SK|Slovakia|Slowakei
SL|Sierra Leone|Sierra Leone
SM|San Marino|San Marino
SN|Senegal|Senegal
SO|Somalia|Somalia
SR|Suriname|Suriname
SS|South Sudan|Südsudan
ST|São Tomé and Príncipe|São Tomé und Príncipe
SV|El Salvador|El Salvador
SX|Sint Maarten|Sint Maarten
SY|Syria|Syrien
SZ|Eswatini|Eswatini
TC|Turks and Caicos Islands|Turks- und Caicosinseln
TD|Chad|Tschad
TF|French Southern Territories|Französische Süd- und Antarktisgebiete
TG|Togo|Togo
TH|Thailand|Thailand
TJ|Tajikistan|Tadschikistan
TK|Tokelau|Tokelau
TL|Timor-Leste|Timor-Leste
TM|Turkmenistan|Turkmenistan
TN|Tunisia|Tunesien
TO|Tonga|Tonga
TR|Türkiye|Türkei
TT|Trinidad and Tobago|Trinidad und Tobago
TV|Tuvalu|Tuvalu
TW|Taiwan|Taiwan
TZ|Tanzania|Tansania
UA|Ukraine|Ukraine
UG|Uganda|Uganda
UM|United States Minor Outlying Islands|Amerikanische Überseeinseln
US|United States|Vereinigte Staaten
UY|Uruguay|Uruguay
UZ|Uzbekistan|Usbekistan
VA|Vatican City|Vatikanstadt
VC|Saint Vincent and the Grenadines|St. Vincent und die Grenadinen
VE|Venezuela|Venezuela
VG|British Virgin Islands|Britische Jungferninseln
VI|U.S. Virgin Islands|Amerikanische Jungferninseln
VN|Vietnam|Vietnam
VU|Vanuatu|Vanuatu
WF|Wallis and Futuna|Wallis und Futuna
WS|Samoa|Samoa
XK|Kosovo|Kosovo
YE|Yemen|Jemen
YT|Mayotte|Mayotte
ZA|South Africa|Südafrika
ZM|Zambia|Sambia
ZW|Zimbabwe|Simbabwe";

/// Every country: (code, English name, German name), by code.
pub fn all() -> impl Iterator<Item = (&'static str, &'static str, &'static str)> {
    TABLE.lines().filter_map(|line| {
        let mut parts = line.split('|');
        Some((parts.next()?, parts.next()?, parts.next()?))
    })
}

/// The name of a country in the language, or the code if unknown.
pub fn name(code: &str, en: bool) -> String {
    all()
        .find(|(c, _, _)| c.eq_ignore_ascii_case(code))
        .map(|(_, e, d)| if en { e } else { d }.to_string())
        .unwrap_or_else(|| code.to_ascii_uppercase())
}

/// Whether `code` is a known country.
pub fn known(code: &str) -> bool {
    all().any(|(c, _, _)| c.eq_ignore_ascii_case(code))
}

#[cfg(test)]
mod tests {
    #[test]
    fn countries_have_codes_and_names() {
        let list: Vec<_> = super::all().collect();
        assert!(list.len() > 240, "{}", list.len());
        assert!(
            list.windows(2).all(|w| w[0].0 < w[1].0),
            "sorted, no duplicates"
        );
        assert!(
            list.iter()
                .all(|(c, e, d)| c.len() == 2 && !e.is_empty() && !d.is_empty())
        );
        assert_eq!(super::name("de", false), "Deutschland");
        assert_eq!(super::name("AT", true), "Austria");
        assert!(super::known("ch") && !super::known("XX"));
    }
}
