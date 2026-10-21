// A synthetic Go sample.
package store

import "fmt"

const maxItems = 128

type Store struct {
	items map[string]int
}

func (s *Store) Put(key string, n int) error {
	if len(s.items) >= maxItems {
		return fmt.Errorf("full: %q\n", key)
	}
	s.items[key] = n
	return nil
}
