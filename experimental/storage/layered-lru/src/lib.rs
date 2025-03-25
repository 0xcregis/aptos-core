use aptos_experimental_layered_map::MapLayer;

pub struct LRULayer<K, V> {
    inner: MapLayer<K, V>,
}

#[cfg(test)]
mod tests {
    #[test]
    fn it_works() {
        assert_eq!(2 + 2, 4);
    }
}
