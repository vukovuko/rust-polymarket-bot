# Polymarket Weather Market Structure

## The 14 Cities

Polymarket lists **exactly 14 cities** for daily high temperature markets.
This is the complete set — other cities (Houston, LA, Denver, etc.) are Kalshi-only.

### US Cities (6) — Fahrenheit, 2F buckets
| City | Slug | Airport | Lat/Lon | Timezone |
|------|------|---------|---------|----------|
| New York | nyc | KLGA (LaGuardia) | 40.78/-73.87 | America/New_York |
| Chicago | chicago | KORD (O'Hare) | 41.97/-87.91 | America/Chicago |
| Miami | miami | KMIA | 25.79/-80.28 | America/New_York |
| Atlanta | atlanta | KATL | 33.64/-84.43 | America/New_York |
| Dallas | dallas | KDAL (Love Field) | 32.85/-96.85 | America/Chicago |
| Seattle | seattle | KSEA | 47.45/-122.31 | America/Los_Angeles |

### International Cities (8) — Celsius, 1C buckets
| City | Slug | Airport | Lat/Lon | Timezone |
|------|------|---------|---------|----------|
| London | london | EGLC (City) | 51.50/-0.05 | Europe/London |
| Paris | paris | LFPG (CDG) | 49.01/2.55 | Europe/Paris |
| Seoul | seoul | RKSI (Incheon) | 37.46/126.44 | Asia/Seoul |
| Toronto | toronto | CYYZ (Pearson) | 43.68/-79.63 | America/Toronto |
| Ankara | ankara | LTAC (Esenboga) | 40.13/32.99 | Europe/Istanbul |
| Buenos Aires | buenos-aires | SAEZ (Ezeiza) | -34.82/-58.54 | America/Argentina/Buenos_Aires |
| Wellington | wellington | NZWN | -41.33/174.81 | Pacific/Auckland |
| Sao Paulo | sao-paulo | SBGR (Guarulhos) | -23.43/-46.47 | America/Sao_Paulo |

## Why Airport Coordinates?

Polymarket resolves against **Weather Underground (WU)** hourly observations.
WU stations are typically at airports. If we forecast for city center coordinates
instead of the airport, we can be off by 1-3F:
- Seoul city center vs Incheon Airport: 50km apart, different microclimate
- Paris city center vs CDG: 25km apart
- Buenos Aires vs Ezeiza: different elevation and urban heat island

## Resolution Source

**Weather Underground (WU)**, NOT NWS CLIs or any other source.
WU can differ from NWS by 1-2F for the same city on the same day.
This is why `check_edges` uses the Weather Company API (WU's backend).

## Bucket Structure

Each city-date has ~9 buckets. Example for NYC on a mild day:
```
Bucket 1:  31F or below     (-inf, 31)
Bucket 2:  32-33F           (32, 33)
Bucket 3:  34-35F           (34, 35)
Bucket 4:  36-37F           (36, 37)
Bucket 5:  38-39F           (38, 39)
Bucket 6:  40-41F           (40, 41)
Bucket 7:  42-43F           (42, 43)
Bucket 8:  44-45F           (44, 45)
Bucket 9:  46F or higher    (46, +inf)
```

US cities: 2F wide buckets. International: 1C wide buckets.
Tail buckets (first and last) are unbounded.

## Volume and Liquidity

- **Per market**: $50K - $240K in volume
- **Best liquidity**: NYC, London, Seoul
- **Under-served**: Ankara, Wellington, Sao Paulo (thinner books = more edge)
- **Daily across all cities**: ~$2M-$5M total volume

## Market Slug Format

```
highest-temperature-in-{city}-on-{month}-{day}-{year}
```
Example: `highest-temperature-in-nyc-on-march-02-2026`

Each slug is an "event" on Polymarket's Gamma API containing 9 binary markets (buckets).

## Lookahead

We fetch markets for **today + 2 days** (3 days total). Markets further out
typically have wider spreads and less volume, but potentially larger edges
since forecasts are less certain and the market may misprice tails more.
