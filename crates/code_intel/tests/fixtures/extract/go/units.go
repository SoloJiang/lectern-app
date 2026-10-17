package units

import (
	"errors"
	"fmt"
)

const Precision = 3

const (
	metresPerFoot = 0.3048
	maxValue      = 1e9
)

var ErrRange = errors.New("out of range")

var (
	defaultUnit = "m"
	conversions = map[string]float64{}
)

type Meters float64

type Feet = float64

type Converter interface {
	Convert(value float64, unit string) (Meters, error)
	Name() string
}

type table struct {
	Name  string
	units map[string]float64
}

func NewTable(name string) *table {
	return &table{Name: name, units: map[string]float64{}}
}

func (t *table) Convert(value float64, unit string) (Meters, error) {
	factor, ok := t.units[unit]
	if !ok {
		return 0, fmt.Errorf("unknown unit %q", unit)
	}
	return check(Meters(value * factor))
}

func (t table) Name() string { return t.Name }

func check(m Meters) (Meters, error) {
	if float64(m) > maxValue {
		return 0, ErrRange
	}
	return m, nil
}

func FromFeet(values ...float64) []Meters {
	out := make([]Meters, 0, len(values))
	for _, v := range values {
		out = append(out, Meters(v*metresPerFoot))
	}
	return out
}
