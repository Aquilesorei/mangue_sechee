//! Whimsical, human-friendly device display name generator.
//! Generates and formats memorable display names for devices and peers
//! (e.g. "Peach", "Sleepy Penguin", "Angry Potato", "Quantum Toaster", etc.)
//! while preserving internal IDs/UUIDs intact under the hood.

use rand::Rng;

pub const ICONIC_NAMES: &[&str] = &[
    "Peach",
    "Sleepy Penguin",
    "Angry Potato",
    "Quantum Toaster",
    "Tiny Dragon",
    "Cosmic Banana",
    "Confused Tomato",
    "Illegal Sandwich",
    "Lonely Router",
    "Suspicious Duck",
    "Velvet Mango",
    "Hyperactive Waffle",
    "Caffeinated Otter",
    "Sneaky Capybara",
    "Bouncing Hedgehog",
    "Turbo Falcon",
    "Mystic Burrito",
    "Cozy Teapot",
    "Dapper Avocado",
    "Sassy Pineapple",
];

pub const ADJECTIVES: &[&str] = &[
    "Sleepy", "Angry", "Quantum", "Tiny", "Cosmic", "Confused", "Illegal",
    "Lonely", "Suspicious", "Bouncing", "Dancing", "Electric", "Turbo",
    "Sneaky", "Velvet", "Golden", "Mystic", "Cozy", "Caffeinated", "Clever",
    "Wobbly", "Dapper", "Jolly", "Radiant", "Chill", "Sassy", "Hyperactive",
    "Galactic", "Polite", "Invisible", "Spicy", "Gentle", "Chunky", "Furious",
    "Zen", "Happy", "Brave", "Curious", "Sunny", "Lucky", "Funky", "Crispy",
];

pub const NOUNS: &[&str] = &[
    "Penguin", "Potato", "Toaster", "Dragon", "Banana", "Tomato", "Sandwich",
    "Router", "Duck", "Peach", "Mango", "Waffle", "Otter", "Koala", "Burrito",
    "Hedgehog", "Capybara", "Falcon", "Muffin", "Biscuit", "Teapot", "Raccoon",
    "Avocado", "Narwhal", "Pineapple", "Gecko", "Hamster", "Donut", "Panda",
    "Fox", "Cactus", "Taco", "Cupcake", "Kitten", "Walrus", "Chameleon",
];

/// Generates a random friendly display name.
pub fn generate_random_name() -> String {
    let mut rng = rand::thread_rng();
    // 35% chance to pick directly from user's curated iconic names
    if rng.gen_bool(0.35) {
        let idx = rng.gen_range(0..ICONIC_NAMES.len());
        ICONIC_NAMES[idx].to_string()
    } else {
        let adj_idx = rng.gen_range(0..ADJECTIVES.len());
        let noun_idx = rng.gen_range(0..NOUNS.len());
        format!("{} {}", ADJECTIVES[adj_idx], NOUNS[noun_idx])
    }
}

/// Generates a friendly display name that is guaranteed NOT to be in `excluded`.
/// Used for decentralized autonomous name negotiation to resolve LAN collisions.
pub fn generate_free_name(excluded: &[String]) -> String {
    let clean_excluded: Vec<String> = excluded
        .iter()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty())
        .collect();

    // Try up to 150 random combinations
    for _ in 0..150 {
        let candidate = generate_random_name();
        if !clean_excluded.iter().any(|e| e == &candidate.to_lowercase()) {
            return candidate;
        }
    }

    // Fallback: If all attempts hit excluded names, append a number
    let base = generate_random_name();
    for i in 2..500 {
        let candidate = format!("{base} {i}");
        if !clean_excluded.iter().any(|e| e == &candidate.to_lowercase()) {
            return candidate;
        }
    }

    let mut rng = rand::thread_rng();
    format!("{base} {}", rng.gen_range(100..999))
}

/// Computes a deterministic friendly display name from an internal ID or UUID.
/// Ensures that a device without an explicit custom name consistently has the same display name.
pub fn name_from_id(id: &str) -> String {
    let clean = id.trim();
    if clean.is_empty() {
        return "Cozy Penguin".to_string();
    }

    // FNV-1a 64-bit hash
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in clean.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }

    // 25% of the time, map to one of the curated iconic names
    if (hash % 4) == 0 {
        let idx = ((hash >> 8) as usize) % ICONIC_NAMES.len();
        return ICONIC_NAMES[idx].to_string();
    }

    let adj_idx = ((hash >> 16) as usize) % ADJECTIVES.len();
    let noun_idx = ((hash >> 24) as usize) % NOUNS.len();
    format!("{} {}", ADJECTIVES[adj_idx], NOUNS[noun_idx])
}

/// Detects whether a string is a raw technical identifier (UUID, IP, placeholder)
/// so the UI can avoid displaying raw UUIDs to the user.
pub fn is_raw_uuid(s: &str) -> bool {
    let t = s.trim();
    if t.is_empty() || t == "primary-peer" || t == "unknown" || t.starts_with("peer-") {
        return true;
    }

    // UUID format check: 8-4-4-4-12 hex characters
    let parts: Vec<&str> = t.split('-').collect();
    if parts.len() == 5
        && parts[0].len() == 8
        && parts[1].len() == 4
        && parts[2].len() == 4
        && parts[3].len() == 4
        && parts[4].len() == 12
        && parts.iter().all(|p| p.chars().all(|c| c.is_ascii_hexdigit()))
    {
        return true;
    }

    // IP address format check
    let host_part = t.split(':').next().unwrap_or(t);
    let ip_octets: Vec<&str> = host_part.split('.').collect();
    if ip_octets.len() == 4 && ip_octets.iter().all(|o| o.parse::<u8>().is_ok()) {
        return true;
    }

    false
}

/// Selects the best human-facing display name.
/// If `custom_display_name` is present and not a raw UUID, uses it.
/// Otherwise falls back to generating a deterministic funny name from `id_or_addr`.
pub fn format_display_name(custom_display_name: Option<&str>, id_or_addr: &str) -> String {
    if let Some(custom) = custom_display_name {
        let trimmed = custom.trim();
        if !trimmed.is_empty() && !is_raw_uuid(trimmed) {
            return trimmed.to_string();
        }
    }
    name_from_id(id_or_addr)
}

/// If `candidate` is present and not a raw UUID, returns it; otherwise computes deterministic name from `id_or_addr`.
pub fn clean_display_name(candidate: &str, id_or_addr: &str) -> String {
    let trimmed = candidate.trim();
    if !trimmed.is_empty() && !is_raw_uuid(trimmed) {
        trimmed.to_string()
    } else {
        name_from_id(id_or_addr)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_uuid_detection() {
        assert!(is_raw_uuid("d8a3835e-25e6-4a07-929d-8e0ec7dd20c7"));
        assert!(is_raw_uuid("8865e9f5-9ecb-4cda-bfa1-6a7d9bc6c06c"));
        assert!(is_raw_uuid("192.168.100.123:24800"));
        assert!(is_raw_uuid("10.0.0.1"));
        assert!(is_raw_uuid("primary-peer"));
        assert!(is_raw_uuid("peer-192_168_1_50"));

        assert!(!is_raw_uuid("Sleepy Penguin"));
        assert!(!is_raw_uuid("Quantum Toaster"));
        assert!(!is_raw_uuid("fedora-laptop"));
        assert!(!is_raw_uuid("Peach"));
    }

    #[test]
    fn test_deterministic_naming() {
        let id = "d8a3835e-25e6-4a07-929d-8e0ec7dd20c7";
        let name1 = name_from_id(id);
        let name2 = name_from_id(id);
        assert_eq!(name1, name2, "Deterministic name must match across calls");
        assert!(!name1.is_empty());
        assert!(!is_raw_uuid(&name1));
    }

    #[test]
    fn test_generate_free_name() {
        let excluded = vec![
            "Peach".to_string(),
            "Sleepy Penguin".to_string(),
            "Quantum Toaster".to_string(),
        ];
        let free = generate_free_name(&excluded);
        assert!(!excluded.contains(&free));
        assert!(!free.is_empty());
    }
}
